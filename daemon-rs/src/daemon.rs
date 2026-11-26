//! Daemon wrapper: device contract (FIFO/meta/conf/status/flock) + AES67 transport
//! from the vendored NSP `nsp_aes67` crate.
//!
//! The C v5.3 daemon is the behavioural reference for everything the device
//! touches: the FIFO is drained eagerly, `live.meta` drives stream changes,
//! `status.json` keeps its schema, and the single-instance lock exits with 3.
//! Packetisation, SDP, SAP, DSCP, PTP and multi-flow packing come from
//! `nsp_aes67` (std/extension flow mode, ptime, L16/L24, QoS).

use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nsp_aes67::ptp;
use nsp_aes67::rtp::{Encoding as RtpEncoding, Flow, MAX_FLOW_CHANNELS};
use nsp_aes67::session::{Config as SessionConfig, Session};
use nsp_aes67::time::{PacketTiming, TimestampMode};

use crate::config::{plan_layout, Config, Encoding, LayoutPlan};
use crate::level::LevelMeter;
use crate::meta::{self, MetaState};
use crate::pcm::{SampleFmt, MAX_CH};
use crate::platform;
use crate::resample::{Passthrough, PolyphaseResampler, Resampler, RubatoResampler};
use crate::source::{self, Wait};
use crate::status::{self, FlowStatus, Snapshot};
use crate::{logln, MAX_RTP_PAYLOAD};

pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_LOCKED: i32 = 3;

/// AES67 media-profile convention: a standard flow carries 1-8 channels.
const STANDARD_FLOW_CHANNELS: usize = 8;

#[derive(Clone, Debug)]
pub struct FlowPlan {
    pub index: usize,
    pub offset: usize,
    pub channels: usize,
    pub dest: SocketAddrV4,
    pub ssrc: u32,
    pub payload_type: u8,
    pub fragment_mtu: usize,
}

#[derive(Clone, Debug)]
pub struct FlowParams {
    pub index: usize,
    pub offset: usize,
    pub channels: usize,
    pub group: Ipv4Addr,
    pub port: u16,
    pub ssrc: u32,
    pub payload_type: u8,
}

#[derive(Clone, Debug)]
pub struct RtpParams {
    pub sample_rate: u32,
    pub channels: usize,
    pub encoding: &'static str,
    pub ptime_us: u32,
    pub split: bool,
    pub mtu: usize,
    pub flows: Vec<FlowParams>,
}

/// Shared handle surface for future control planes (RAVENNA discovery, RTSP):
/// read the SDP(s), query the RTP parameters, and gate the sender.
pub struct SharedState {
    sdp: Mutex<Vec<String>>,
    params: Mutex<Option<RtpParams>>,
    send_enabled: AtomicBool,
}

impl SharedState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sdp: Mutex::new(Vec::new()),
            params: Mutex::new(None),
            send_enabled: AtomicBool::new(true),
        })
    }
    pub fn sdp(&self) -> Option<String> {
        self.sdps().into_iter().next()
    }
    pub fn sdps(&self) -> Vec<String> {
        self.sdp.lock().map(|guard| guard.clone()).unwrap_or_default()
    }
    pub fn rtp_params(&self) -> Option<RtpParams> {
        self.params.lock().ok().and_then(|guard| guard.clone())
    }
    pub fn set_send_enabled(&self, enabled: bool) {
        self.send_enabled.store(enabled, Ordering::Release);
    }
    pub fn send_enabled(&self) -> bool {
        self.send_enabled.load(Ordering::Acquire)
    }
    fn publish(&self, sdps: Vec<String>, params: RtpParams) {
        if let Ok(mut guard) = self.sdp.lock() {
            *guard = sdps;
        }
        if let Ok(mut guard) = self.params.lock() {
            *guard = Some(params);
        }
    }
}

pub struct Daemon {
    cfg: Config,
    shared: Arc<SharedState>,
    /// Effective PTP mode after startup: local / unicast / multicast.
    ptp_mode: &'static str,
}

impl Daemon {
    pub fn new(cfg: Config) -> Result<Self, String> {
        cfg.validate()?;
        Ok(Self {
            cfg,
            shared: SharedState::new(),
            ptp_mode: "local",
        })
    }

    pub fn shared(&self) -> Arc<SharedState> {
        Arc::clone(&self.shared)
    }

    pub fn run(&mut self) -> i32 {
        match self.run_inner() {
            Ok(code) => code,
            Err((code, message)) => {
                logln!("ERROR {message}");
                code
            }
        }
    }

    fn run_inner(&mut self) -> Result<i32, (i32, String)> {
        let cfg = self.cfg.clone();
        if let Some(path) = cfg.log.as_deref() {
            crate::logging::set_file(path)
                .map_err(|err| (EXIT_USAGE, format!("open log {path}: {err}")))?;
        }
        platform::init_timer_resolution();
        platform::install_stop_handlers();
        for note in &cfg.notes {
            logln!("WARN {note}");
        }
        if matches!(cfg.codec.as_str(), "bits" | "auto" | "eac3" | "truehd") {
            logln!(
                "codec={}: consuming the HAL tee raw bitstream (live.bits); truehd -> dolby-pipeline, ac3/eac3 -> daemon decode",
                cfg.codec
            );
        }
        let group_ip = cfg.group_ip();
        let local_ip = cfg
            .local_ip
            .unwrap_or_else(|| platform::detect_local_ip(group_ip, cfg.port).unwrap_or(Ipv4Addr::UNSPECIFIED));
        let encoding = if cfg.encoding == Encoding::L24 {
            RtpEncoding::L24
        } else {
            RtpEncoding::L16
        };
        logln!(
            "v{} start fifo={} meta={} sdp={} status={} conf={} lock={} group={} port={} ptime_us={} maxch={} layout={} codec={} encoding={} pt={} mtu={} split={} ch={} rate={} seconds={:.0} local_ip={} resampler={:?} ptp={} ptp_gm={:?} ptp_domain={} sap={}",
            env!("CARGO_PKG_VERSION"),
            cfg.fifo,
            cfg.meta,
            cfg.sdp,
            cfg.status,
            cfg.conf,
            cfg.lock,
            cfg.group,
            cfg.port,
            cfg.ptime_us,
            cfg.maxch,
            cfg.layout,
            cfg.codec,
            encoding.codec(),
            cfg.payload_type(),
            cfg.mtu,
            !cfg.no_split,
            cfg.force_ch,
            cfg.force_rate,
            cfg.seconds,
            local_ip,
            cfg.resampler,
            cfg.ptp,
            cfg.ptp_master,
            cfg.ptp_domain,
            cfg.sap
        );
        if cfg.pt != 0 {
            let expected = if cfg.encoding == Encoding::L24 { 97 } else { 96 };
            if cfg.pt != expected {
                logln!(
                    "WARN pt={} with encoding={} (AES67 default mapping is 97=L24 / 96=L16); receivers that derive the sample width from the PT will misread this stream",
                    cfg.pt,
                    encoding.codec()
                );
            }
        }
        let _lock = platform::acquire_instance_lock(&cfg.lock).map_err(|err| {
            (
                EXIT_LOCKED,
                format!(
                    "another daemon already holds {} ({}), exiting",
                    cfg.lock, err
                ),
            )
        })?;
        match status::init_socket(status::SOCKET_NAME) {
            Ok(()) => {
                logln!("status socket @{}", status::SOCKET_NAME);
                // Rust engine is socket-only: drop the legacy JSON file so the UI cannot read stale data.
                let _ = std::fs::remove_file(&cfg.status);
            }
            Err(err) => logln!("WARN status socket bind failed: {err}"),
        }
        if cfg.sched_fifo > 0 {
            let rc = platform::sched_fifo(cfg.sched_fifo);
            logln!("SCHED_FIFO prio={} rc={}", cfg.sched_fifo, rc);
        }
        if cfg.http_port > 0 {
            spawn_sdp_http_server(cfg.http_port, cfg.sdp.clone(), local_ip);
        }
        // codec=bits: consume the HAL tee raw compressed ES (live.bits) and
        // decode E-AC-3 / AC-3 core PCM. This path never reads live.pcm and is
        // selected explicitly so the PCM transport contract stays untouched.
        if cfg.codec == "auto" {
            return self.run_auto(&cfg);
        }
        if matches!(cfg.codec.as_str(), "bits" | "eac3" | "truehd") {
            return self.run_bits(&cfg, false);
        }
        self.run_pcm(&cfg, false)
    }

    /// Keep a silent AES67 session up while no live source exists.
    /// SAP only describes a running session, so announcing while idle needs a real
    /// RTP stream: this sends packet-timed silence at the configured layout/PT and
    /// lets aes67-daemon (or any SAP browser) discover and configure the source at
    /// any time, not just while music/movie is playing.
    fn run_idle_session(&mut self, cfg: &Config) -> Result<i32, (i32, String)> {
        let layout_id = cfg.layout.trim();
        if layout_id.is_empty() {
            return Err((EXIT_USAGE, "idle session needs a configured layout".to_string()));
        }
        let channels = crate::config::layout_channels(layout_id)
            .unwrap_or_else(|| cfg.maxch.max(2))
            .max(1);
        let layout_plan = rendered_layout_plan(layout_id, layout_id, channels);
        let timing = PacketTiming::new(crate::OUT_RATE, cfg.ptime_us)
            .ok_or((EXIT_USAGE, format!("unsupported ptime {} us", cfg.ptime_us)))?;
        let encoding = if cfg.encoding == Encoding::L24 {
            RtpEncoding::L24
        } else {
            RtpEncoding::L16
        };
        let group_ip = cfg.group_ip();
        let local_ip = cfg.local_ip.unwrap_or_else(|| {
            platform::detect_local_ip(group_ip, cfg.port).unwrap_or(Ipv4Addr::UNSPECIFIED)
        });
        let plan = plan_flows(cfg, &timing, encoding, layout_plan.channels, platform::now_ns())
            .map_err(|message| (EXIT_USAGE, message))?;
        let mut ring_cfg = cfg.clone();
        ring_cfg.buffer_ms = ring_cfg.buffer_ms.max(2_000);
        ring_cfg.prebuffer_ms = ring_cfg.prebuffer_ms.min(ring_cfg.buffer_ms / 4);
        let (session, ptp_mode) = start_session_with_ptp_fallback(
            &ring_cfg,
            local_ip,
            &timing,
            encoding,
            &layout_plan,
            &plan,
            self.ptp_mode,
        )
        .map_err(|message| (EXIT_USAGE, message))?;
        self.ptp_mode = ptp_mode;
        publish_sdp(&session, cfg, &plan, &self.shared);
        logln!(
            "IDLE session up layout={} ch={} flows={} ptp={} (silent RTP keeps SAP discoverable)",
            layout_plan.output_layout,
            channels,
            plan.len(),
            self.ptp_mode
        );
        let shape = StreamShape {
            channels,
            source_channels: channels,
            map: (0..channels).collect(),
            layout: layout_plan.output_layout.clone(),
            rate: crate::OUT_RATE,
            fmt: SampleFmt::S16,
            stream_id: 0,
        };
        let frames = timing.samples_per_packet().max(1) as usize;
        let zeros = vec![0.0f32; frames * channels];
        let mut first = 0u64;
        let mut last_status_ns = platform::now_ns();
        loop {
            if platform::stop_requested() {
                break;
            }
            if auto_source(cfg) != AutoSource::Idle {
                logln!("IDLE session released: a live source appeared");
                break;
            }
            let mut rc = 0;
            let mut attempts = 0u32;
            loop {
                rc = unsafe { session.submit(first, zeros.as_ptr(), frames, channels) };
                if rc != -7 || attempts >= 500 {
                    break;
                }
                attempts += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            if rc == 0 {
                first += frames as u64;
            }
            let now = platform::now_ns();
            if now - last_status_ns > 1_000_000_000 {
                last_status_ns = now;
                let parts = StatusParts {
                    rs_in: crate::OUT_RATE as u64,
                    rs_out: crate::OUT_RATE as u64,
                    submitted: first,
                    levels: [-120.0f64; MAX_CH],
                    src_active: false,
                    src_age_ms: 0,
                    src_codec: "none",
                    src_rate: crate::OUT_RATE,
                    _marker: std::marker::PhantomData,
                };
                write_status(&session, cfg, &plan, &parts, 0, &shape, self.ptp_mode, None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let rc = session.stop(2_000);
        if rc != 0 {
            logln!("WARN IDLE session stop code={rc}");
        }
        Ok(EXIT_OK)
    }

    /// codec=auto: probe both HAL tee sources and run whichever one is live.
    /// live.bits (compressed ES -> TrueHD/AC3 decode) wins while it is active;
    /// otherwise live.pcm (already-decoded PCM) is rendered to the configured
    /// layout. When both are quiet the session is released and the router waits,
    /// so the UI shows "waiting for a source" instead of an endless silent stream.
    fn run_auto(&mut self, cfg: &Config) -> Result<i32, (i32, String)> {
        logln!(
            "AUTO router start: probing {} (compressed) and {} (decoded PCM)",
            cfg.bits_meta,
            cfg.meta
        );
        loop {
            if platform::stop_requested() {
                return Ok(EXIT_OK);
            }
            match auto_source(cfg) {
                AutoSource::Bits => {
                    logln!("AUTO source=bits (HAL tee compressed ES)");
                    match self.run_bits(cfg, true) {
                        Ok(code) => {
                            if code != EXIT_OK {
                                return Ok(code);
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err((EXIT_USAGE, message)) => return Err((EXIT_USAGE, message)),
                        Err((code, message)) => {
                            logln!("WARN AUTO bits path exited code={code}: {message}");
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    }
                }
                AutoSource::Pcm => {
                    logln!("AUTO source=pcm (HAL decoded PCM)");
                    match self.run_pcm(cfg, true) {
                        Ok(code) => {
                            if code != EXIT_OK {
                                return Ok(code);
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err((EXIT_USAGE, message)) => return Err((EXIT_USAGE, message)),
                        Err((code, message)) => {
                            logln!("WARN AUTO pcm path exited code={code}: {message}");
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    }
                }
                AutoSource::Idle => {
                    // Keep a silent session up while idle: SAP only describes a
                    // running session, and controllers must be able to discover /
                    // configure the source at any time, not just during playback.
                    match self.run_idle_session(cfg) {
                        Ok(_) => {}
                        Err((EXIT_USAGE, message)) => return Err((EXIT_USAGE, message)),
                        Err((code, message)) => {
                            logln!("WARN AUTO idle session failed code={code}: {message}");
                            write_idle_status(cfg, self.ptp_mode);
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    }
                }
            }
        }
    }

    /// PCM / live.pcm path. The session is (re)built on live.meta changes and the
    /// optional ROAR render stage carries gain, downmix and the look-ahead limiter.
    /// `auto` = the router owns the loop: return when live.bits becomes the live
    /// source, or after the stream has been silent for `AUTO_IDLE`.
    fn run_pcm(&mut self, cfg: &Config, auto: bool) -> Result<i32, (i32, String)> {
        let cfg = cfg.clone();
        let group_ip = cfg.group_ip();
        let local_ip = cfg.local_ip.unwrap_or_else(|| {
            platform::detect_local_ip(group_ip, cfg.port).unwrap_or(Ipv4Addr::UNSPECIFIED)
        });
        let encoding = if cfg.encoding == Encoding::L24 {
            RtpEncoding::L24
        } else {
            RtpEncoding::L16
        };
        let mut source = source::open(&cfg.fifo)
            .map_err(|err| (EXIT_USAGE, format!("open fifo {} failed: {err}", cfg.fifo)))?;

        let mut meta = MetaState::default();
        meta::parse_meta(&cfg.meta, &mut meta);

        let timing = PacketTiming::new(crate::OUT_RATE, cfg.ptime_us)
            .ok_or((EXIT_USAGE, format!("unsupported ptime {} us", cfg.ptime_us)))?;

        // Engine state (rebuilt on live.meta stream/channel/rate changes).
        let mut session: Option<Session> = None;
        let mut plan: Vec<FlowPlan> = Vec::new();
        let mut submitter = Submitter::new(2);
        submitter.set_gain(10f32.powf(cfg.gain_db.clamp(-60.0, 12.0) / 20.0));
        let mut resampler: Box<dyn Resampler> = Box::new(Passthrough);
        // PCM -> ROAR render stage (None = legacy strict intersection path).
        let mut render_stage: Option<PcmRenderStage> = None;
        let mut current = StreamShape {
            channels: 2,
            source_channels: 2,
            map: vec![0, 1],
            layout: String::new(),
            rate: crate::OUT_RATE,
            fmt: SampleFmt::S16,
            stream_id: -1,
        };
        let mut carry: Vec<u8> = Vec::with_capacity(128);
        let mut meter = LevelMeter::default();
        // Optional diagnostic: dump the exact PCM frames handed to the session
        // (after resampling) as 24-bit big-endian, the same format as the RTP payload.
        let mut pcm_dump = if cfg.dump_pcm.is_empty() {
            None
        } else {
            match std::fs::File::create(&cfg.dump_pcm) {
                Ok(file) => Some(file),
                Err(err) => {
                    logln!("WARN create --dump-pcm {} failed: {err}", cfg.dump_pcm);
                    None
                }
            }
        };

        let mut bytes_read: u64 = 0;
        let mut reads: u64 = 0;
        let mut eofs: u64 = 0;
        let mut last_data_ns = platform::now_ns();
        // Audible-silence diagnostics: log when the HAL tee stops writing and when it resumes.
        let mut silent_since_ns: i64 = 0;
        let mut silent_warned = false;
        let mut fifo_last = 0usize;
        let mut fifo_max = 0usize;
        let mut maxch_warned = false;
        let mut fmt_warned = false;
        let mut submit_warned = false;
        let mut pcm_render_warned = false;
        let start_ns = platform::now_ns();
        let mut last_meta_ns = 0i64;
        let mut last_status_ns = 0i64;
        let mut last_stat_ns = start_ns;
        let mut chunk: Vec<u8> = Vec::with_capacity(65_536);

        let result: Result<(), (i32, String)> = (|| {
            loop {
                if platform::stop_requested() {
                    break;
                }
                if auto && bits_stream_active(&cfg) {
                    logln!("AUTO switch: live.bits became the live source; releasing the PCM session");
                    break;
                }
                if auto && session.is_some() && source_idle(last_data_ns, cfg.auto_idle_ms) {
                    logln!("AUTO idle: no live.pcm bytes for {} ms; releasing the PCM session", cfg.auto_idle_ms);
                    break;
                }
                let now = platform::now_ns();
                if cfg.seconds > 0.0 && (now - start_ns) as f64 / 1e9 >= cfg.seconds {
                    break;
                }
                if now - last_meta_ns > 250_000_000 {
                    meta::parse_meta(&cfg.meta, &mut meta);
                    last_meta_ns = platform::now_ns();
                }
                let mut source_channels = if cfg.force_ch > 0 {
                    cfg.force_ch
                } else {
                    meta.channels
                };
                let rate = if cfg.force_rate > 0 {
                    cfg.force_rate
                } else {
                    meta.rate
                };
                let fmt = meta.fmt;
                let stream_id = meta.stream_id;
                source_channels = source_channels.clamp(1, MAX_CH);
                if cfg.layout.trim().is_empty() && source_channels > cfg.maxch {
                    if !maxch_warned {
                        maxch_warned = true;
                        logln!(
                            "WARN meta ch={} > maxch={}, clamping (configure --maxch to allow more)",
                            source_channels,
                            cfg.maxch
                        );
                    }
                    source_channels = cfg.maxch;
                }
                // daemon.conf layout is the target/maximum layout declared to the backend.
                // PCM is rendered through ROAR when possible: the real source layout is declared
                // as a channel-based element and ROAR maps it to the configured target layout
                // (IAMF downmix for legal pairs, EAR otherwise), so the wire layout is the target.
                // Without a usable target/source layout the legacy strict intersection is kept
                // (missing target channels not filled, extra source channels dropped, no downmix).
                let source_layout_id =
                    match meta.layout.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        Some(id) if crate::config::layout_channels(id) == Some(source_channels) => {
                            id.to_string()
                        }
                        _ => crate::config::derive_layout(source_channels).to_string(),
                    };
                let layout_plan = classify_pcm_layout(
                    &cfg,
                    &source_layout_id,
                    source_channels,
                    meta.layout.as_deref(),
                    &mut render_stage,
                    &mut pcm_render_warned,
                )
                .map_err(|message| (EXIT_USAGE, message))?;
                let channels = layout_plan.channels;
                if rate != 44_100 && rate != 48_000 {
                    if !fmt_warned {
                        fmt_warned = true;
                        logln!("WARN unsupported source rate {rate}; waiting for 44100/48000");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let changed = session.is_none()
                    || current.channels != channels
                    || current.source_channels != source_channels
                    || current.layout != layout_plan.output_layout
                    || current.map != layout_plan.map
                    || current.rate != rate
                    || current.stream_id != stream_id;
                if changed {
                    if let Some(old) = session.take() {
                        let rc = old.stop(2_000);
                        if rc != 0 {
                            logln!("WARN previous session stop code={rc}");
                        }
                    }
                    plan = plan_flows(&cfg, &timing, encoding, channels, now)
                        .map_err(|message| (EXIT_USAGE, message))?;
                    resampler = make_resampler(&cfg, rate, source_channels)
                        .map_err(|message| (EXIT_USAGE, message))?;
                    let (fresh, ptp_mode) = start_session_with_ptp_fallback(
                        &cfg,
                        local_ip,
                        &timing,
                        encoding,
                        &layout_plan,
                        &plan,
                        self.ptp_mode,
                    )
                    .map_err(|message| (EXIT_USAGE, message))?;
                    self.ptp_mode = ptp_mode;
                    logln!(
                        "META src_ch={} out_ch={} layout={} target={} source={} rate={} fmt={} sid={} flows={} encoding={} ptime_us={}",
                        source_channels,
                        channels,
                        layout_plan.output_layout,
                        if layout_plan.target.is_empty() { "-" } else { &layout_plan.target },
                        if layout_plan.source.is_empty() { "unknown" } else { &layout_plan.source },
                        rate,
                        fmt.name(),
                        stream_id,
                        plan.len(),
                        encoding.codec(),
                        cfg.ptime_us
                    );
                    if !layout_plan.missing.is_empty() {
                        logln!(
                            "INFO layout {} + source {} -> {} ({}ch); target channels not present, not filled: {}",
                            if layout_plan.target.is_empty() { "-" } else { &layout_plan.target },
                            if layout_plan.source.is_empty() { "unknown" } else { &layout_plan.source },
                            layout_plan.output_layout,
                            channels,
                            layout_plan.missing.join(",")
                        );
                    }
                    if !layout_plan.dropped.is_empty() {
                        logln!(
                            "WARN layout {} + source {} -> {} ({}ch); dropping source channels outside target (no downmix): {}",
                            if layout_plan.target.is_empty() { "-" } else { &layout_plan.target },
                            if layout_plan.source.is_empty() { "unknown" } else { &layout_plan.source },
                            layout_plan.output_layout,
                            channels,
                            layout_plan.dropped.join(",")
                        );
                    }
                    publish_sdp(&fresh, &cfg, &plan, &self.shared);
                    session = Some(fresh);
                    submitter = Submitter::new(layout_plan.source_channels);
                    submitter.set_gain(10f32.powf(cfg.gain_db.clamp(-60.0, 12.0) / 20.0));
                    if let Some(stage) = render_stage.as_mut() {
                        stage.reset();
                    }
                    carry.clear();
                    meter.reset();
                    current = StreamShape {
                        channels,
                        source_channels,
                        map: layout_plan.map.clone(),
                        layout: layout_plan.output_layout.clone(),
                        rate,
                        fmt,
                        stream_id,
                    };
                } else if current.fmt != fmt {
                    // Format-only change: buffers must be re-aligned but the RTP
                    // stream identity (SSRC/SDP) stays valid.
                    logln!("META fmt={} (frames re-aligned, stream kept)", fmt.name());
                    current.fmt = fmt;
                    carry.clear();
                    resampler.reset();
                }
                let Some(active) = session.as_ref() else {
                    continue;
                };
                let frame_bytes = fmt.bytes_per_sample() * source_channels;
                fifo_last = source.available();
                if fifo_last > fifo_max {
                    fifo_max = fifo_last;
                }
                chunk.clear();
                let (read, eof) = source
                    .read_available(&mut chunk)
                    .map_err(|err| (EXIT_USAGE, format!("read {} failed: {err}", cfg.fifo)))?;
                if read > 0 {
                    bytes_read += read as u64;
                    reads += 1;
                    last_data_ns = platform::now_ns();
                    carry.extend_from_slice(&chunk);
                    let mut consumed = 0usize;
                    while carry.len() - consumed >= frame_bytes {
                        let mut frame = [0_i32; MAX_CH];
                        fmt.decode_frame(
                            &carry[consumed..consumed + frame_bytes],
                            source_channels,
                            &mut frame,
                        );
                        consumed += frame_bytes;
                        if !self.shared.send_enabled() {
                            continue;
                        }
                        feed_frame(
                            &frame,
                            source_channels,
                            rate,
                            resampler.as_mut(),
                            &mut meter,
                            &mut submitter,
                            render_stage.as_mut(),
                            &mut pcm_dump,
                        );
                    }
                    if consumed > 0 {
                        carry.drain(..consumed);
                    }
                    if carry.len() > 65_536 {
                        // Resynchronise rather than grow without bound.
                        carry.clear();
                    }
                }
                if eof {
                    eofs += 1;
                    flush_frames(
                        source_channels,
                        resampler.as_mut(),
                        &mut meter,
                        &mut submitter,
                        render_stage.as_mut(),
                        &mut pcm_dump,
                    );
                    std::thread::sleep(Duration::from_millis(20));
                } else if platform::now_ns() - last_data_ns > 500_000_000 {
                    // Regular-file host source: no write-end HUP exists, so a
                    // half second of silence after real data means "end of
                    // take" and the SRC tail can be flushed exactly like the
                    // device FIFO path does on POLLHUP.
                    flush_frames(
                        source_channels,
                        resampler.as_mut(),
                        &mut meter,
                        &mut submitter,
                        render_stage.as_mut(),
                        &mut pcm_dump,
                    );
                }
                if let Err(err) = submitter.flush(active) {
                    if !submit_warned {
                        submit_warned = true;
                        logln!("WARN PCM submit failed: {err}");
                    }
                }
                let now = platform::now_ns();
                if now - last_status_ns > 1_000_000_000 {
                    last_status_ns = now;
                    write_status(
                        active,
                        &cfg,
                        &plan,
                        &seat(&meter, &resampler, &submitter, age_ms(last_data_ns), current.rate),
                        fifo_last,
                        &current,
                        self.ptp_mode,
                        None,
                    );
                    meter.reset();
                }
                let gap_ms = age_ms(last_data_ns);
                if gap_ms > 500 && !silent_warned {
                    silent_warned = true;
                    silent_since_ns = now;
                    logln!("WARN PCM source silent for {} ms (HAL tee stopped writing live.pcm; sending silence)", gap_ms);
                } else if gap_ms < 50 && silent_warned {
                    silent_warned = false;
                    let ms = if silent_since_ns > 0 { (now - silent_since_ns) / 1_000_000 } else { 0 };
                    logln!("PCM source resumed after {} ms of silence", ms);
                }
                if now - last_stat_ns > 10_000_000_000 {
                    last_stat_ns = now;
                    let counters = active.counters();
                    logln!(
                        "STAT packets={} bytes={} send_err={} drops={} silent={} late={} read_bytes={} reads={} eofs={} ring_fill={} fifo_bytes={} fifo_max={} ch={} rate={} rs_in={} rs_out={} flows={} locked={} pcm_render={} render_blocks={} limited={} peak={:.3}",
                        counters.packets,
                        counters.bytes,
                        counters.errors,
                        counters.dropped,
                        counters.silent,
                        counters.late,
                        bytes_read,
                        reads,
                        eofs,
                        active.ring_frames(),
                        fifo_last,
                        fifo_max,
                        source_channels,
                        rate,
                        resampler.rs_in(),
                        resampler.rs_out(),
                        plan.len(),
                        counters.locked,
                        u8::from(render_stage.is_some()),
                        render_stage.as_ref().map_or(0, |stage| stage.rendered_blocks),
                        render_stage.as_ref().map_or(0, |stage| stage.limited),
                        render_stage.as_ref().map_or(0.0, |stage| stage.peak)
                    );
                }
                if read == 0 && !eof {
                    match source.wait(1) {
                        Wait::Readable | Wait::Timeout => {}
                        Wait::EndOfStream => {
                            eofs += 1;
                            flush_frames(
                                source_channels,
                                resampler.as_mut(),
                                &mut meter,
                                &mut submitter,
                                render_stage.as_mut(),
                                &mut pcm_dump,
                            );
                        }
                    }
                }
            }
            Ok(())
        })();

        // Exit path: flush the SRC tail and give the session a moment to play it.
        if let Some(active) = session.as_ref() {
            let source_channels = current.source_channels;
            flush_frames(
                source_channels,
                resampler.as_mut(),
                &mut meter,
                &mut submitter,
                render_stage.as_mut(),
                &mut pcm_dump,
            );
            let _ = submitter.flush(active);
            write_status(
                active,
                &cfg,
                &plan,
                &seat(&meter, &resampler, &submitter, age_ms(last_data_ns), current.rate),
                fifo_last,
                &current,
                self.ptp_mode,
                None,
            );
            if result.is_ok() {
                std::thread::sleep(Duration::from_millis(50));
            }
            let counters = active.counters();
            logln!(
                "EXIT packets={} bytes={} send_err={} dropped={} silent={} late={} read_bytes={} reads={} eofs={} rs_in={} rs_out={}",
                counters.packets,
                counters.bytes,
                counters.errors,
                counters.dropped,
                counters.silent,
                counters.late,
                bytes_read,
                reads,
                eofs,
                resampler.rs_in(),
                resampler.rs_out()
            );
        }
        if let Some(active) = session.take() {
            let rc = active.stop(2_000);
            if rc != 0 {
                logln!("WARN session stop code={rc}");
            }
        }
        result.map(|_| EXIT_OK)
    }

    /// codec=bits: decode the HAL tee compressed ES (live.bits) to PCM and feed
    /// the same AES67 session. Core AC-3/E-AC-3 only; no DSP, no down/upmix.
    fn run_bits(&mut self, cfg: &Config, auto: bool) -> Result<i32, (i32, String)> {
        use std::collections::VecDeque;
        use std::sync::atomic::AtomicU64;

        // Open the HAL tee FIFO unconditionally at startup: meta can be stale
        // or missing until playback starts, but the bytes on live.bits are the
        // real trigger. Bytes are buffered until the codec is identified, so a
        // HAL restart/phantom meta cannot make us miss the first TrueHD AU.
        let mut bits_source = source::open(&cfg.bits)
            .map_err(|err| (EXIT_USAGE, format!("open bits {} failed: {err}", cfg.bits)))?;
        let mut prefetched: Vec<u8> = Vec::new();
        let mut last_data_ns = platform::now_ns();
        let mut reopen_warned = false;
        let mut last_reopen_ns = 0i64;
        // GUI start and movie playback are separate user actions: while
        // waiting for a stream keep the process (and status heartbeat) alive
        // indefinitely. The explicit --seconds test mode still bounds the wait.
        let deadline = if cfg.seconds > 0.0 {
            platform::now_ns() + (cfg.seconds * 1e9) as i64
        } else {
            i64::MAX
        };
        let mut bits_meta = None;
        let mut last_heartbeat = 0i64;
        let mut warned_rate = false;
        while platform::now_ns() < deadline {
            if platform::stop_requested() {
                return Ok(EXIT_OK);
            }
            if auto && !bits_stream_active(cfg) {
                // Nothing is arriving on live.bits: hand control back to the router
                // (it either starts the PCM path or idles) instead of blocking here.
                return Ok(EXIT_OK);
            }
            let now = platform::now_ns();
            // The TV app treats a stale status.json as "daemon not running".
            if now - last_heartbeat > 500_000_000 {
                last_heartbeat = now;
                write_idle_status(cfg, if cfg.ptp { "multicast" } else { "local" });
            }
            // Keep the FIFO drained while the codec is unknown (bounded memory).
            if prefetched.len() < 8 * 1024 * 1024 {
                let mut chunk: Vec<u8> = Vec::with_capacity(65_536);
                let (read, eof) = bits_source
                    .read_available(&mut chunk)
                    .map_err(|err| (EXIT_USAGE, format!("read bits {} failed: {err}", cfg.bits)))?;
                if read > 0 {
                    prefetched.extend_from_slice(&chunk);
                    last_data_ns = now;
                }
                if eof && now - last_reopen_ns > 1_000_000_000 {
                    // HAL proxy rebuild replaces the FIFO inode; reopen it.
                    // Throttle to once per second while no writer is attached.
                    last_reopen_ns = now;
                    std::thread::sleep(Duration::from_millis(50));
                    match source::open(&cfg.bits) {
                        Ok(fresh) => {
                            bits_source = fresh;
                            logln!("INFO live.bits EOF; reopened {}", cfg.bits);
                        }
                        Err(err) => {
                            if !reopen_warned {
                                reopen_warned = true;
                                logln!("WARN live.bits reopen {} failed: {err}", cfg.bits);
                            }
                        }
                    }
                }
            }
            let meta_candidate = meta::parse_bits_meta(&cfg.bits_meta);
            let meta_ok = match meta_candidate.as_ref() {
                Some(meta) => {
                    let stale = meta.is_stale();
                    if !stale && meta.is_dolby_es() && meta.rate != 48_000 && !warned_rate {
                        warned_rate = true;
                        logln!(
                            "WARN ignoring stale live.bits.meta {} at {} Hz (only 48 kHz; waiting for the real stream)",
                            meta.label(),
                            meta.rate
                        );
                    }
                    !stale
                        && (meta.is_truehd()
                            || (meta.is_dolby_es() && meta.rate == 48_000))
                }
                None => false,
            };
            let detected = if prefetched.len() >= 8 {
                dolby_pipeline::bits::detect_codec(&prefetched)
            } else {
                dolby_pipeline::bits::Codec::Unknown
            };
            let bytes_truehd = detected == dolby_pipeline::bits::Codec::TrueHd;
            let want_truehd = matches!(cfg.codec.as_str(), "truehd" | "auto" | "bits");
            if want_truehd && (bytes_truehd || meta_candidate.as_ref().is_some_and(meta::BitsMeta::is_truehd)) {
                let bits_meta = meta_candidate.unwrap_or_else(|| meta::BitsMeta {
                    stream_id: 0,
                    fmt: 0x0E00_0000,
                    fmt_name: "IEC61937".to_string(),
                    codec_guess: "truehd".to_string(),
                    rate: 48_000,
                    chmask: 0,
                    ts_ms: 0,
                    open_ts_ms: 0,
                    bytes: prefetched.len() as u64,
                    dropped: 0,
                });
                return self.run_truehd(cfg, auto, &bits_meta, bits_source, prefetched);
            }
            // codec=truehd must never fall through to the AC3/EAC3 path on a
            // transient 48 kHz phantom meta; only a real TrueHD byte stream (or
            // a truehd meta) may start the pipeline.
            if meta_ok && cfg.codec != "truehd" {
                if let Some(meta) = meta_candidate {
                    bits_meta = Some(meta);
                    break;
                }
            }
            let _ = last_data_ns;
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(bits_meta) = bits_meta else {
            return Err((
                EXIT_USAGE,
                format!(
                    "codec={} waiting for a real 48 kHz AC3/EAC3/TrueHD stream on {} (no valid live.bits.meta yet)",
                    cfg.codec, cfg.bits
                ),
            ));
        };
        if bits_meta.rate != 48_000 && !bits_meta.is_truehd() {
            return Err((
                EXIT_USAGE,
                format!(
                    "no 48 kHz AC3/EAC3 stream in live.bits.meta (last rate {}); wait for playback or set decode=truehd",
                    bits_meta.rate
                ),
            ));
        }
        let source_channels = bits_meta.channels().clamp(1, MAX_CH);
        let derive = crate::config::derive_layout(source_channels);
        let source_layout = if derive.is_empty() { None } else { Some(derive) };
        let layout_plan = plan_layout(&cfg.layout, source_channels, source_layout)
            .map_err(|message| (EXIT_USAGE, message))?;
        let timing = PacketTiming::new(crate::OUT_RATE, cfg.ptime_us)
            .ok_or((EXIT_USAGE, format!("unsupported ptime {} us", cfg.ptime_us)))?;
        let encoding = if cfg.encoding == Encoding::L24 {
            RtpEncoding::L24
        } else {
            RtpEncoding::L16
        };
        let local_ip = cfg.local_ip.unwrap_or_else(|| {
            platform::detect_local_ip(cfg.group_ip(), cfg.port).unwrap_or(Ipv4Addr::UNSPECIFIED)
        });
        let mut ring_cfg = cfg.clone();
        ring_cfg.buffer_ms = ring_cfg.buffer_ms.max(2_000);
        ring_cfg.prebuffer_ms = ring_cfg.prebuffer_ms.min(ring_cfg.buffer_ms / 4);
        let plan = plan_flows(cfg, &timing, encoding, layout_plan.channels, platform::now_ns())
            .map_err(|message| (EXIT_USAGE, message))?;
        let (session, ptp_mode) = start_session_with_ptp_fallback(
            &ring_cfg,
            local_ip,
            &timing,
            encoding,
            &layout_plan,
            &plan,
            self.ptp_mode,
        )
        .map_err(|message| (EXIT_USAGE, message))?;
        self.ptp_mode = ptp_mode;
        let shape = StreamShape {
            channels: layout_plan.channels,
            source_channels: layout_plan.source_channels,
            map: layout_plan.map.clone(),
            layout: layout_plan.output_layout.clone(),
            rate: 48_000,
            fmt: SampleFmt::S16,
            stream_id: bits_meta.stream_id,
        };
        logln!(
            "BITS start codec={} fmt=0x{:08x} src_ch={} out_ch={} layout={} target={} source={} flows={} encoding={} ptime_us={} bits={}",
            bits_meta.label(),
            bits_meta.fmt,
            source_channels,
            layout_plan.channels,
            layout_plan.output_layout,
            if layout_plan.target.is_empty() { "-" } else { &layout_plan.target },
            if layout_plan.source.is_empty() { "unknown" } else { &layout_plan.source },
            plan.len(),
            encoding.codec(),
            cfg.ptime_us,
            cfg.bits
        );
        publish_sdp(&session, cfg, &plan, &self.shared);
        let mut decoder = crate::bitstream::BitsDecoder::new();
        if !prefetched.is_empty() {
            decoder.push_bytes(&prefetched);
            prefetched.clear();
        }
        let mut dump_bits_file = if cfg.dump_bits.is_empty() {
            None
        } else {
            Some(std::fs::File::create(&cfg.dump_bits).map_err(|err| {
                (EXIT_USAGE, format!("create --dump-bits {} failed: {err}", cfg.dump_bits))
            })?)
        };
        let mut dump_pcm_file = if cfg.dump_pcm.is_empty() {
            None
        } else {
            Some(std::fs::File::create(&cfg.dump_pcm).map_err(|err| {
                (EXIT_USAGE, format!("create --dump-pcm {} failed: {err}", cfg.dump_pcm))
            })?)
        };
        let ring_capacity = cfg.buffer_ms.max(2_000) as usize * crate::OUT_RATE as usize / 1000;
        let mut queue: VecDeque<f32> = VecDeque::new();
        let mut first = 0u64;
        let mut submitted = 0u64;
        let mut bytes_read = 0u64;
        let mut frames_decoded = 0u64;
        let mut nonzero_samples = 0u64;
        let mut decoder_errors = 0u64;
        let mut short_warned = false;
        let mut submit_warned = false;
        let mut meter = LevelMeter::default();
        let chunk_frames = 480usize;
        let chunk_samples = chunk_frames * source_channels;
        let start_ns = platform::now_ns();
        let mut last_data_ns = start_ns;
        let mut last_status_ns = start_ns;
        let mut buf: Vec<u8> = Vec::with_capacity(65_536);
        let mut eofs = 0u64;
        let _ = AtomicU64::new(0);
        let mut submit_into = |session: &Session,
                               queue: &mut VecDeque<f32>,
                               first: &mut u64,
                               submitted: &mut u64,
                               warned: &mut bool|
         -> bool {
            while queue.len() >= chunk_samples {
                let chunk: Vec<f32> = queue.iter().take(chunk_samples).copied().collect();
                // E-AC-3 bursts often decode faster than real time; wait for
                // ring space instead of dropping most of the burst (QUEUE_FULL).
                let mut waited = 0u32;
                while session.ring_frames() + chunk_frames + 480 > ring_capacity
                    && waited < 2_500
                {
                    std::thread::sleep(Duration::from_millis(2));
                    waited += 1;
                }
                let mut rc = 0;
                let mut attempts = 0u32;
                loop {
                    rc = unsafe {
                        session.submit(*first, chunk.as_ptr(), chunk_frames, source_channels)
                    };
                    if rc != -7 || attempts >= 500 {
                        break;
                    }
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(2));
                }
                if rc != 0 {
                    if !*warned {
                        *warned = true;
                        logln!("WARN BITS session.submit code={rc}; dropping decoded PCM chunk");
                    }
                    for _ in 0..chunk_samples {
                        queue.pop_front();
                    }
                    *first += chunk_frames as u64;
                    continue;
                }
                for _ in 0..chunk_samples {
                    queue.pop_front();
                }
                *first += chunk_frames as u64;
                *submitted += chunk_frames as u64;
            }
            true
        };
        let result: Result<(), (i32, String)> = (|| {
            loop {
                if platform::stop_requested() {
                    break;
                }
                if auto && !bits_stream_active(cfg) && (pcm_stream_active(cfg) || source_idle(last_data_ns, cfg.auto_idle_ms)) {
                    logln!("AUTO switch: live.bits went quiet; releasing the bits session");
                    break;
                }
                let now = platform::now_ns();
                if cfg.seconds > 0.0 && (now - start_ns) as f64 / 1e9 >= cfg.seconds {
                    break;
                }
                buf.clear();
                let (read, eof) = if !prefetched.is_empty() {
                    // Bytes already drained from live.bits before the codec was
                    // identified (possibly before HAL restart); process them first.
                    let first = std::mem::take(&mut prefetched);
                    buf.extend_from_slice(&first);
                    (first.len(), false)
                } else {
                    bits_source.read_available(&mut buf).map_err(|err| {
                        (EXIT_USAGE, format!("read bits {} failed: {err}", cfg.bits))
                    })?
                };
                if read > 0 {
                    bytes_read += read as u64;
                    last_data_ns = now;
                    if let Some(file) = dump_bits_file.as_mut() {
                        use std::io::Write as _;
                        let _ = file.write_all(&buf);
                    }
                    decoder.push_bytes(&buf);
                }
                if eof {
                    eofs += 1;
                }
                loop {
                    let Some(frame) = decoder.next_frame() else {
                        break;
                    };
                    frames_decoded += (frame.samples.len() / frame.channels) as u64;
                    if frame.channels != source_channels {
                        if !short_warned {
                            short_warned = true;
                            logln!(
                                "WARN BITS decoded {} channels but live.bits.meta implies {}; dropping frame",
                                frame.channels,
                                source_channels
                            );
                        }
                        decoder_errors += 1;
                        continue;
                    }
                    let decode_gain = 10f32.powf(cfg.gain_db.clamp(-60.0, 12.0) / 20.0);
                    for samples in frame.samples.chunks_exact(frame.channels) {
                        let mut pcm = [0_i32; MAX_CH];
                        for (channel, &sample) in samples.iter().enumerate() {
                            pcm[channel] = (sample * decode_gain * 8_388_608.0)
                                .round()
                                .clamp(-8_388_608.0, 8_388_607.0)
                                as i32;
                        }
                        meter.add_frame(&pcm, source_channels);
                        if let Some(file) = dump_pcm_file.as_mut() {
                            use std::io::Write as _;
                            let mut bytes = Vec::with_capacity(source_channels * 3);
                            for channel in 0..source_channels {
                                if pcm[channel] != 0 {
                                    nonzero_samples += 1;
                                }
                                let be = pcm[channel].to_be_bytes();
                                bytes.extend_from_slice(&be[1..4]);
                            }
                            let _ = file.write_all(&bytes);
                        }
                        for channel in 0..source_channels {
                            queue.push_back(pcm[channel] as f32 / 8_388_608.0);
                        }
                    }
                }
                submit_into(&session, &mut queue, &mut first, &mut submitted, &mut submit_warned);
                if queue.len() > 48_000 * source_channels * 4 {
                    let excess = queue.len() / source_channels - 48_000 * 2;
                    for _ in 0..(excess * source_channels) {
                        queue.pop_front();
                    }
                    if !submit_warned {
                        submit_warned = true;
                        logln!("WARN BITS decoded queue overflow; dropping oldest PCM");
                    }
                }
                let now = platform::now_ns();
                if now - last_status_ns > 1_000_000_000 {
                    last_status_ns = now;
                    let parts = StatusParts {
                        rs_in: crate::OUT_RATE as u64,
                        rs_out: crate::OUT_RATE as u64,
                        submitted,
                        levels: meter.dbfs(),
                        src_active: now - last_data_ns < 1_000_000_000,
                        src_age_ms: age_ms(last_data_ns),
                        src_codec: "eac3",
                        src_rate: 48_000,
                        _marker: std::marker::PhantomData,
                    };
                    write_status(&session, cfg, &plan, &parts, queue.len() / source_channels, &shape, self.ptp_mode, None);
                    meter.reset();
                    logln!(
                        "BITS STAT bytes={} frames_decoded={} submitted={} queue_frames={} decoder_errors={} nonzero_samples={} send_err={} ring_fill={} drops={} silent={}",
                        bytes_read,
                        frames_decoded,
                        submitted,
                        queue.len() / source_channels,
                        decoder_errors,
                        nonzero_samples,
                        session.counters().errors,
                        session.ring_frames(),
                        session.counters().dropped,
                        session.counters().silent
                    );
                }
                if read == 0 {
                    if eof && now - last_data_ns > 500_000_000 && queue.is_empty() {
                        break;
                    }
                    match bits_source.wait(2) {
                        Wait::Readable | Wait::Timeout => {}
                        Wait::EndOfStream => {
                            eofs += 1;
                        }
                    }
                }
            }
            Ok(())
        })();
        submit_into(&session, &mut queue, &mut first, &mut submitted, &mut submit_warned);
        let parts = StatusParts {
            rs_in: crate::OUT_RATE as u64,
            rs_out: crate::OUT_RATE as u64,
            submitted,
            levels: meter.dbfs(),
            src_active: age_ms(last_data_ns) < 1_000,
            src_age_ms: age_ms(last_data_ns),
            src_codec: "eac3",
            src_rate: 48_000,
            _marker: std::marker::PhantomData,
        };
        write_status(&session, cfg, &plan, &parts, 0, &shape, self.ptp_mode, None);
        let counters = session.counters();
        logln!(
            "BITS EXIT bytes={} decoded_frames={} submitted={} decoder_errors={} nonzero_samples={} eofs={} send_err={} drops={} silent={}",
            bytes_read,
            nonzero_samples,
            frames_decoded,
            submitted,
            decoder_errors,
            eofs,
            counters.errors,
            counters.dropped,
            counters.silent
        );
        let rc = session.stop(2_000);
        if rc != 0 {
            logln!("WARN BITS session stop code={rc}");
        }
        result.map(|_| EXIT_OK)
    }

    /// codec=truehd: decode the HAL tee TrueHD/Atmos ES through dolby-pipeline
    /// (TrueHD -> objects/OAMD -> ROAR -> 480-frame blocks) and feed the same
    /// AES67 session. live.bits bytes are passed through unchanged (the 4-byte
    /// AU header is ES syntax, not a container header).
    fn run_truehd(
        &mut self,
        cfg: &Config,
        auto: bool,
        bits_meta: &meta::BitsMeta,
        mut bits_source: source::Source,
        mut prefetched: Vec<u8>,
    ) -> Result<i32, (i32, String)> {
        let target = cfg.layout.trim().to_string();
        if target.is_empty() {
            return Err((
                EXIT_USAGE,
                "codec=truehd needs --target-layout/--layout= (5.1.2|7.1.2|7.1.4|9.1.6)".to_string(),
            ));
        }
        if target == "4.1.2" {
            return Err((
                EXIT_USAGE,
                "target layout 4.1.2 is not supported by the ROAR validated patch; use 5.1.2/7.1.2/7.1.4/9.1.6 and let the downstream processor downmix".to_string(),
            ));
        }
        let mut pipeline = dolby_pipeline::ObjectPipeline::new_with(&target, cfg.limiter_db)
            .map_err(|err| (EXIT_USAGE, format!("truehd pipeline init target={target} failed: {err}")))?;
        pipeline.set_dialnorm_enabled(cfg.dialnorm);
        pipeline.set_user_gain_db(cfg.gain_db);
        let timing = PacketTiming::new(crate::OUT_RATE, cfg.ptime_us)
            .ok_or((EXIT_USAGE, format!("unsupported ptime {} us", cfg.ptime_us)))?;
        let encoding = if cfg.encoding == Encoding::L24 {
            RtpEncoding::L24
        } else {
            RtpEncoding::L16
        };
        let local_ip = cfg.local_ip.unwrap_or_else(|| {
            platform::detect_local_ip(cfg.group_ip(), cfg.port).unwrap_or(Ipv4Addr::UNSPECIFIED)
        });
        let mut ring_cfg = cfg.clone();
        ring_cfg.buffer_ms = ring_cfg.buffer_ms.max(2_000);
        ring_cfg.prebuffer_ms = ring_cfg.prebuffer_ms.min(ring_cfg.buffer_ms / 4);
        let mut dump_bits_file = if cfg.dump_bits.is_empty() {
            None
        } else {
            Some(std::fs::File::create(&cfg.dump_bits).map_err(|err| {
                (EXIT_USAGE, format!("create --dump-bits {} failed: {err}", cfg.dump_bits))
            })?)
        };
        let mut dump_pcm_file = if cfg.dump_pcm.is_empty() {
            None
        } else {
            Some(std::fs::File::create(&cfg.dump_pcm).map_err(|err| {
                (EXIT_USAGE, format!("create --dump-pcm {} failed: {err}", cfg.dump_pcm))
            })?)
        };
        let mut session: Option<Session> = None;
        let mut plan: Vec<FlowPlan> = Vec::new();
        let mut shape: Option<StreamShape> = None;
        let mut first = 0u64;
        let mut submitted = 0u64;
        let mut bytes_read = 0u64;
        let mut blocks_out = 0u64;
        let mut frames_out = 0u64;
        let mut submit_errors = 0u64;
        let mut eofs = 0u64;
        let mut last_objects = 0usize;
        let mut pipeline_errors = 0u64;
        let mut reopen_warned = false;
        let mut last_reopen_ns = 0i64;
        let mut preroll: Vec<dolby_pipeline::Block> = Vec::new();
        let mut started = false;
        let mut pending: Vec<dolby_pipeline::Block> = Vec::new();
        let ring_capacity = ring_cfg.buffer_ms as usize * crate::OUT_RATE as usize / 1000;
        let mut push_ms: Vec<f64> = Vec::new();
        let mut meter = LevelMeter::default();
        let start_ns = platform::now_ns();
        let mut last_data_ns = start_ns;
        let mut last_status_ns = start_ns;
        let mut buf: Vec<u8> = Vec::with_capacity(65_536);
        let loop_result: Result<(), (i32, String)> = (|| {
            loop {
                if platform::stop_requested() {
                    break;
                }
                if auto && !bits_stream_active(cfg) && (pcm_stream_active(cfg) || source_idle(last_data_ns, cfg.auto_idle_ms)) {
                    logln!("AUTO switch: live.bits went quiet; releasing the bits session");
                    break;
                }
                let now = platform::now_ns();
                if cfg.seconds > 0.0 && (now - start_ns) as f64 / 1e9 >= cfg.seconds {
                    break;
                }
                buf.clear();
                let (read, eof) = if !prefetched.is_empty() {
                    // Bytes drained before the TrueHD codec was identified
                    // (possibly before a HAL restart): process them first, in
                    // bounded chunks so one decode/extract error cannot throw
                    // away a whole multi-MB prefetch.
                    let take = prefetched.len().min(65_536);
                    let first: Vec<u8> = prefetched.drain(..take).collect();
                    let count = first.len();
                    buf.extend_from_slice(&first);
                    (count, false)
                } else {
                    bits_source.read_available(&mut buf).map_err(|err| {
                        (EXIT_USAGE, format!("read bits {} failed: {err}", cfg.bits))
                    })?
                };
                if read > 0 {
                    bytes_read += read as u64;
                    last_data_ns = now;
                    if let Some(file) = dump_bits_file.as_mut() {
                        use std::io::Write as _;
                        let _ = file.write_all(&buf);
                    }

                    let push_start = platform::now_ns();
                    let blocks = match pipeline.push_bits(&buf) {
                        Ok(blocks) => blocks,
                        Err(err) => {
                            pipeline_errors += 1;
                            logln!(
                                "WARN truehd pipeline error #{}: {err}; resetting pipeline",
                                pipeline_errors
                            );
                            pipeline = dolby_pipeline::ObjectPipeline::new_with(&target, cfg.limiter_db)
                                .map_err(|reset_err| {
                                    (
                                        EXIT_USAGE,
                                        format!("truehd pipeline reset failed: {reset_err}"),
                                    )
                                })?;
                            pipeline.set_dialnorm_enabled(cfg.dialnorm);
                            pipeline.set_user_gain_db(cfg.gain_db);
                            preroll.clear();
                            pending.clear();
                            started = false;
                            continue;
                        }
                    };
                    // Per-published-block decode+render cost (one push call can
                    // complete several 480-frame blocks).
                    if !blocks.is_empty() && push_ms.len() < 8192 {
                        let elapsed_ms = (platform::now_ns() - push_start) as f64 / 1e6;
                        push_ms.push(elapsed_ms / blocks.len() as f64);
                    }
                    for block in blocks {
                        // Atmos TrueHD often starts with a few bed-only 480-frame
                        // blocks before the first OAMD arrives. Publishing 7.1 and
                        // then switching SDP to 7.1.4 a moment later is bad for a
                        // receiver that reads SDP once, so collect 20 blocks
                        // (~0.2 s) and start on the settled actual layout.
                        if !started {
                            preroll.push(block);
                            if preroll.len() < 20 {
                                continue;
                            }
                            started = true;
                            pending = std::mem::take(&mut preroll);
                            let final_layout = pending
                                .last()
                                .map(|item| item.actual.id().to_string())
                                .unwrap_or_default();
                            let before = pending.len();
                            pending.retain(|item| item.actual.id() == final_layout);
                            if before > pending.len() {
                                logln!(
                                    "WARN TRUEHD startup preroll dropped {} leading blocks whose actual layout differed from {}",
                                    before - pending.len(),
                                    final_layout
                                );
                            }
                        } else {
                            pending.clear();
                            pending.push(block);
                        }
                        for block in pending.drain(..) {
                        let channels = block.channels.len();
                        let actual_id = block.actual.id().to_string();
                        last_objects = block.object_count;
                        let rebuild = shape
                            .as_ref()
                            .map_or(true, |s| s.channels != channels || s.layout != actual_id);
                        if rebuild {
                            if let Some(old) = session.take() {
                                let rc = old.stop(2_000);
                                if rc != 0 {
                                    logln!("WARN TRUEHD session stop code={rc}");
                                }
                            }
                            let layout_plan = plan_layout(&cfg.layout, channels, Some(&actual_id))
                                .map_err(|message| (EXIT_USAGE, message))?;
                            let new_plan = plan_flows(
                                cfg,
                                &timing,
                                encoding,
                                layout_plan.channels,
                                platform::now_ns(),
                            )
                            .map_err(|message| (EXIT_USAGE, message))?;
                            let (fresh, ptp_mode) = start_session_with_ptp_fallback(
                                &ring_cfg,
                                local_ip,
                                &timing,
                                encoding,
                                &layout_plan,
                                &new_plan,
                                self.ptp_mode,
                            )
                            .map_err(|message| (EXIT_USAGE, message))?;
                            self.ptp_mode = ptp_mode;
                            publish_sdp(&fresh, cfg, &new_plan, &self.shared);
                            logln!(
                                "TRUEHD layout actual={} target={} ch={} flows={} objects={} encoding={} ptime_us={} codec={}",
                                layout_plan.output_layout,
                                if layout_plan.target.is_empty() { "-" } else { &layout_plan.target },
                                layout_plan.channels,
                                new_plan.len(),
                                last_objects,
                                encoding.codec(),
                                cfg.ptime_us,
                                cfg.codec
                            );
                            plan = new_plan;
                            shape = Some(StreamShape {
                                channels,
                                source_channels: channels,
                                map: layout_plan.map.clone(),
                                layout: actual_id.clone(),
                                rate: 48_000,
                                fmt: SampleFmt::S16,
                                stream_id: bits_meta.stream_id,
                            });
                            session = Some(fresh);
                            first = 0;
                        }
                        let Some(active) = session.as_ref() else {
                            continue;
                        };
                        let frames = block.frames.max(1);
                        let mut chunk: Vec<f32> = Vec::with_capacity(frames * channels);
                        for frame in 0..frames {
                            let mut pcm = [0_i32; MAX_CH];
                            for channel in 0..channels {
                                let sample = block.channels[channel][frame].clamp(-1.0, 1.0);
                                chunk.push(sample);
                                pcm[channel] = (sample * 8_388_608.0)
                                    .round()
                                    .clamp(-8_388_608.0, 8_388_607.0)
                                    as i32;
                            }
                            meter.add_frame(&pcm, channels);
                        }
                        // The pipeline decodes faster than real time (~2x on
                        // armv7). When the AES67 ring is full, retry instead of
                        // dropping the block: this back-pressures the FIFO read
                        // loop, which is the product-level flow control.
                        // Wait for ring space before submitting so the retry
                        // path does not inflate the session "dropped" counter.
                        let mut waited = 0u32;
                        while active.ring_frames() + frames + 480 > ring_capacity && waited < 2_500 {
                            std::thread::sleep(Duration::from_millis(2));
                            waited += 1;
                        }
                        let mut rc = 0;
                        let mut attempts = 0u32;
                        loop {
                            rc = unsafe { active.submit(first, chunk.as_ptr(), frames, channels) };
                            if rc != -7 || attempts >= 500 {
                                break;
                            }
                            attempts += 1;
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        if rc != 0 {
                            submit_errors += 1;
                            if submit_errors <= 3 {
                                logln!(
                                    "WARN TRUEHD submit code={rc} frames={frames} ch={channels} attempts={attempts}"
                                );
                            }
                            continue;
                        }
                        if let Some(file) = dump_pcm_file.as_mut() {
                            use std::io::Write as _;
                            let mut bytes = Vec::with_capacity(chunk.len() * 3);
                            for sample in &chunk {
                                let value = (sample * 8_388_608.0)
                                    .round()
                                    .clamp(-8_388_608.0, 8_388_607.0)
                                    as i32;
                                let be = value.to_be_bytes();
                                bytes.extend_from_slice(&be[1..4]);
                            }
                            let _ = file.write_all(&bytes);
                        }
                        first += frames as u64;
                        submitted += frames as u64;
                        blocks_out += 1;
                        frames_out += frames as u64;
                        }
                    }
                }
                if eof {
                    eofs += 1;
                }
                let now = platform::now_ns();
                if now - last_status_ns > 1_000_000_000 {
                    last_status_ns = now;
                    if let (Some(active), Some(current)) = (session.as_ref(), shape.as_ref()) {
                        let parts = StatusParts {
                            rs_in: 48_000,
                            rs_out: 48_000,
                            submitted,
                            levels: meter.dbfs(),
                            src_active: now - last_data_ns < 1_000_000_000,
                            src_age_ms: age_ms(last_data_ns),
                            src_codec: "truehd",
                            src_rate: 48_000,
                            _marker: std::marker::PhantomData,
                        };
                        let stats = pipeline.decode_stats();
                        let render = pipeline.render_stats();
                        let (push_p50, push_p99) = push_percentiles(&push_ms);
                        let bits_view = status::BitsStatus {
                            object_count: last_objects,
                            decode_render_ms_p50: push_p50,
                            decode_render_ms_p99: push_p99,
                            decode_errors: stats.decode_errors,
                            extract_errors: stats.extract_errors,
                            limited: render.limited_samples,
                            out_peak: render.out_peak,
                            dialnorm_db: pipeline.dialnorm_gain_db(),
                            _marker: std::marker::PhantomData,
                        };
                        write_status(active, cfg, &plan, &parts, 0, current, self.ptp_mode, Some(&bits_view));
                        meter.reset();
                        logln!(
                            "TRUEHD STAT bytes={} blocks={} frames={} objects={} send_err={} ring_fill={} decode_errors={} extract_errors={} limited={} out_peak={:.3} limiter_delay={} dialnorm={:.2}dB",
                            bytes_read,
                            blocks_out,
                            frames_out,
                            last_objects,
                            active.counters().errors,
                            active.ring_frames(),
                            stats.decode_errors,
                            stats.extract_errors,
                            render.limited_samples,
                            render.out_peak,
                            render.limiter_delay_samples,
                            pipeline.dialnorm_gain_db()
                        );
                    } else {
                        write_idle_status(cfg, self.ptp_mode);
                    }
                }
                if read == 0 {
                    if eof {
                        if cfg.seconds > 0.0 && now - last_data_ns > 2_000_000_000 {
                            break;
                        }
                        // HAL proxy rebuild replaces the FIFO inode; reopen and
                        // keep waiting instead of exiting. Throttle to once per
                        // second while no writer is attached.
                        if now - last_reopen_ns > 1_000_000_000 {
                            last_reopen_ns = now;
                            std::thread::sleep(Duration::from_millis(50));
                            match source::open(&cfg.bits) {
                            Ok(fresh) => {
                                bits_source = fresh;
                                logln!("INFO live.bits EOF; reopened {}", cfg.bits);
                            }
                            Err(err) => {
                                if !reopen_warned {
                                    reopen_warned = true;
                                    logln!("WARN live.bits reopen {} failed: {err}", cfg.bits);
                                }
                            }
                            }
                        }
                    }
                    match bits_source.wait(2) {
                        Wait::Readable | Wait::Timeout => {}
                        Wait::EndOfStream => {
                            eofs += 1;
                        }
                    }
                }
            }
            Ok(())
        })();
        if let (Some(active), Some(current)) = (session.as_ref(), shape.as_ref()) {
            let parts = StatusParts {
                rs_in: 48_000,
                rs_out: 48_000,
                submitted,
                levels: meter.dbfs(),
                src_active: age_ms(last_data_ns) < 1_000,
                src_age_ms: age_ms(last_data_ns),
                src_codec: "truehd",
                src_rate: 48_000,
                _marker: std::marker::PhantomData,
            };
            let stats = pipeline.decode_stats();
            let render = pipeline.render_stats();
            let (push_p50, push_p99) = push_percentiles(&push_ms);
            let bits_view = status::BitsStatus {
                object_count: last_objects,
                decode_render_ms_p50: push_p50,
                decode_render_ms_p99: push_p99,
                decode_errors: stats.decode_errors,
                extract_errors: stats.extract_errors,
                limited: render.limited_samples,
                out_peak: render.out_peak,
                dialnorm_db: pipeline.dialnorm_gain_db(),
                _marker: std::marker::PhantomData,
            };
            write_status(active, cfg, &plan, &parts, 0, current, self.ptp_mode, Some(&bits_view));
        }
        push_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p50 = percentile(&push_ms, 0.50);
        let p99 = percentile(&push_ms, 0.99);
        logln!(
            "TRUEHD EXIT bytes={} blocks={} frames={} submitted_frames={} submit_errors={} pipeline_errors={} eofs={} push_p50_ms={:.3} push_p99_ms={:.3}",
            bytes_read,
            blocks_out,
            frames_out,
            submitted,
            submit_errors,
            pipeline_errors,
            eofs,
            p50,
            p99
        );
        let stats = pipeline.decode_stats();
        logln!(
            "TRUEHD STATS decode_aus={} aus_skipped_pre_sync={} extract_errors={} parse_errors={} decode_errors={} oamd_payloads={} discarded_samples={}",
            stats.aus_decoded,
            stats.aus_skipped_pre_sync,
            stats.extract_errors,
            stats.parse_errors,
            stats.decode_errors,
            stats.oamd_payloads,
            stats.discarded_samples
        );
        if let Some(active) = session.take() {
            let rc = active.stop(2_000);
            if rc != 0 {
                logln!("WARN TRUEHD session stop code={rc}");
            }
        }
        loop_result.map(|_| EXIT_OK)
    }

}

fn push_percentiles(samples: &[f64]) -> (f64, f64) {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (percentile(&sorted, 0.50), percentile(&sorted, 0.99))
}

fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() as f64 * q) as usize).min(values.len() - 1);
    values[index]
}

struct StreamShape {
    /// Actual output channels after the layout intersection (what SDP/status
    /// report and what the RTP flows carry).
    channels: usize,
    /// Source block channels (what live.meta reports and the FIFO carries).
    source_channels: usize,
    /// Output channel -> source channel mapping used by the vendor ring.
    map: Vec<usize>,
    /// Actual output layout name (target/source intersection result).
    layout: String,
    rate: u32,
    fmt: SampleFmt,
    stream_id: i64,
}

/// Lightweight view for status snapshots.
struct StatusParts<'a> {
    rs_in: u64,
    rs_out: u64,
    submitted: u64,
    levels: [f64; MAX_CH],
    src_active: bool,
    src_age_ms: u64,
    src_codec: &'static str,
    src_rate: u32,
    _marker: std::marker::PhantomData<&'a ()>,
}

/// Milliseconds since the last source byte, saturating at 0 for clock resets.
fn age_ms(last_data_ns: i64) -> u64 {
    let now = platform::now_ns();
    (now - last_data_ns).max(0) as u64 / 1_000_000
}

fn seat<'a>(
    meter: &LevelMeter,
    resampler: &Box<dyn Resampler>,
    submitter: &Submitter,
    src_age_ms: u64,
    src_rate: u32,
) -> StatusParts<'a> {
    StatusParts {
        rs_in: resampler.rs_in(),
        rs_out: resampler.rs_out(),
        submitted: submitter.submitted,
        levels: meter.dbfs(),
        src_active: src_age_ms < 1_000,
        src_age_ms,
        src_codec: "pcm",
        src_rate,
        _marker: std::marker::PhantomData,
    }
}

struct Submitter {
    pending: Vec<f32>,
    frames: usize,
    first: u64,
    channels: usize,
    submitted: u64,
    /// Linear output trim for the strict (non-rendered) PCM path; the ROAR render
    /// stage applies the same trim before its limiter instead.
    gain: f32,
}

impl Submitter {
    fn new(channels: usize) -> Self {
        Self {
            pending: Vec::new(),
            frames: 0,
            first: 0,
            channels,
            submitted: 0,
            gain: 1.0,
        }
    }

    fn set_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() { gain.max(0.0) } else { 1.0 };
    }

    fn push(&mut self, frame: &[i32; MAX_CH], channels: usize) {
        let gain = self.gain;
        for c in 0..channels.min(frame.len()) {
            self.pending.push(frame[c] as f32 / 8_388_608.0 * gain);
        }
        self.frames += 1;
    }

    /// Append one already-rendered planar f32 block (target layout, ROAR limiter output).
    fn push_planar(&mut self, block: &[Vec<f32>], frames: usize) {
        for frame in 0..frames {
            for channel in 0..self.channels.min(block.len()) {
                self.pending.push(block[channel][frame]);
            }
        }
        self.frames += frames;
    }

    fn flush(&mut self, session: &Session) -> Result<(), String> {
        while self.frames > 0 {
            // 480 frames = 10 ms at 48 kHz, well below the ring capacity.
            let chunk = self.frames.min(480);
            let slice_len = chunk * self.channels;
            let mut rc;
            let mut retries = 0u32;
            loop {
                rc = unsafe {
                    session.submit(
                        self.first,
                        self.pending.as_ptr(),
                        chunk,
                        self.channels,
                    )
                };
                if rc == 0 {
                    break;
                }
                // -7 = QUEUE_FULL: vendor ring momentarily full (session prebuffer or
                // a scheduling burst). Backpressure and retry the SAME frame index;
                // advancing the clock past unsubmitted audio would make every later
                // submit fail and degrade the channel into permanent silence.
                if rc == -7 && retries < 2_000 {
                    retries += 1;
                    std::thread::sleep(Duration::from_micros(500));
                    continue;
                }
                return Err(format!("session.submit code={rc}"));
            }
            self.first += chunk as u64;
            self.submitted += chunk as u64;
            self.pending.drain(..slice_len);
            self.frames -= chunk;
        }
        Ok(())
    }
}
/// PCM -> ROAR render stage: accumulates 480-frame blocks in source channel order, renders them
/// to the configured target layout (channel-based element + IAMF downmix / EAR fallback) and hands
/// target-layout planar f32 to the submitter. The ROAR look-ahead limiter is inside the renderer.
struct PcmRenderStage {
    renderer: dolby_pipeline::PcmRenderer,
    source_layout_id: String,
    target_layout_id: String,
    in_planar: Vec<Vec<f32>>,
    channels_in: usize,
    channels_out: usize,
    frames: usize,
    filled: usize,
    warned: bool,
    rendered_blocks: u64,
    limited: u64,
    peak: f32,
}

impl PcmRenderStage {
    fn new(
        source_layout: &str,
        target_layout: &str,
        limiter_db: f32,
        gain_db: f32,
    ) -> Result<Self, String> {
        let mut config = dolby_pipeline::PcmConfig::new(source_layout, target_layout, 480);
        config.limiter_threshold_db = limiter_db.clamp(-24.0, 0.0);
        let mut renderer =
            dolby_pipeline::PcmRenderer::new(&config).map_err(|err| err.to_string())?;
        renderer.set_input_gain(10f32.powf(gain_db.clamp(-60.0, 12.0) / 20.0));
        let channels_in = renderer.source_layout().len();
        let channels_out = renderer.target_layout().len();
        Ok(Self {
            renderer,
            source_layout_id: source_layout.to_string(),
            target_layout_id: target_layout.to_string(),
            in_planar: vec![vec![0.0; 480]; channels_in],
            channels_in,
            channels_out,
            frames: 480,
            filled: 0,
            warned: false,
            rendered_blocks: 0,
            limited: 0,
            peak: 0.0,
        })
    }

    fn matches(&self, source_layout_id: &str, target_layout_id: &str) -> bool {
        self.source_layout_id == source_layout_id && self.target_layout_id == target_layout_id
    }

    fn reset(&mut self) {
        self.filled = 0;
    }

    fn push(
        &mut self,
        frame: &[i32; MAX_CH],
        channels: usize,
        meter: &mut LevelMeter,
        submitter: &mut Submitter,
    ) {
        if channels != self.channels_in {
            if !self.warned {
                self.warned = true;
                logln!(
                    "WARN PCM render: frame has {channels} channels, source layout expects {}",
                    self.channels_in
                );
            }
            return;
        }
        for (channel, samples) in self.in_planar.iter_mut().enumerate() {
            samples[self.filled] = frame[channel] as f32 / 8_388_608.0;
        }
        self.filled += 1;
        if self.filled == self.frames {
            self.emit(meter, submitter);
        }
    }

    fn finish(&mut self, meter: &mut LevelMeter, submitter: &mut Submitter) {
        if self.filled == 0 {
            return;
        }
        for channel in &mut self.in_planar {
            for sample in channel[self.filled..self.frames].iter_mut() {
                *sample = 0.0;
            }
        }
        self.emit(meter, submitter);
    }

    fn emit(&mut self, meter: &mut LevelMeter, submitter: &mut Submitter) {
        let frames = self.frames;
        match self.renderer.render_block(&self.in_planar) {
            Ok(block) => {
                submitter.push_planar(&block, frames);
                meter.add_planar(&block, frames);
                self.rendered_blocks += 1;
                self.limited = self.renderer.limited_samples();
                self.peak = self.renderer.out_peak();
            }
            Err(err) => {
                if !self.warned {
                    self.warned = true;
                    logln!("WARN PCM render failed: {err}");
                }
            }
        }
        self.filled = 0;
    }
}

fn feed_frame(
    frame: &[i32; MAX_CH],
    channels: usize,
    rate: u32,
    resampler: &mut dyn Resampler,
    meter: &mut LevelMeter,
    submitter: &mut Submitter,
    mut render_stage: Option<&mut PcmRenderStage>,
    dump: &mut Option<std::fs::File>,
) {
    let mut dump_frame = |out: &[i32; MAX_CH]| {
        if let Some(file) = dump.as_mut() {
            use std::io::Write as _;
            let mut bytes = Vec::with_capacity(channels * 3);
            for channel in 0..channels {
                let be = out[channel].to_be_bytes();
                bytes.extend_from_slice(&be[1..4]);
            }
            let _ = file.write_all(&bytes);
        }
    };
    if rate == crate::OUT_RATE {
        dump_frame(frame);
        match render_stage.as_mut() {
            Some(stage) => stage.push(frame, channels, meter, submitter),
            None => {
                meter.add_frame(frame, channels);
                submitter.push(frame, channels);
            }
        }
    } else {
        let mut sink = |out: &[i32; MAX_CH]| {
            dump_frame(out);
            match render_stage.as_mut() {
                Some(stage) => stage.push(out, channels, meter, submitter),
                None => {
                    meter.add_frame(out, channels);
                    submitter.push(out, channels);
                }
            }
        };
        resampler.push(frame, channels, &mut sink);
    }
}

fn flush_frames(
    channels: usize,
    resampler: &mut dyn Resampler,
    meter: &mut LevelMeter,
    submitter: &mut Submitter,
    mut render_stage: Option<&mut PcmRenderStage>,
    dump: &mut Option<std::fs::File>,
) {
    {
        let mut sink = |out: &[i32; MAX_CH]| {
            if let Some(file) = dump.as_mut() {
                use std::io::Write as _;
                let mut bytes = Vec::with_capacity(channels * 3);
                for channel in 0..channels {
                    let be = out[channel].to_be_bytes();
                    bytes.extend_from_slice(&be[1..4]);
                }
                let _ = file.write_all(&bytes);
            }
            match render_stage.as_mut() {
                Some(stage) => stage.push(out, channels, meter, submitter),
                None => {
                    meter.add_frame(out, channels);
                    submitter.push(out, channels);
                }
            }
        };
        resampler.flush(channels, &mut sink);
    }
    if let Some(stage) = render_stage.as_mut() {
        stage.finish(meter, submitter);
    }
}

/// Pick the PCM layout plan: ROAR rendering when a target layout is configured and both layouts
/// are ROAR-supported (channel-based element with the real source layout), otherwise the legacy
/// strict intersection (no downmix, drop extra source channels).
fn classify_pcm_layout(
    cfg: &Config,
    source_layout_id: &str,
    source_channels: usize,
    meta_layout: Option<&str>,
    render_stage: &mut Option<PcmRenderStage>,
    warned: &mut bool,
) -> Result<LayoutPlan, String> {
    if cfg.pcm_render && !cfg.layout.trim().is_empty() {
        let target = cfg.layout.trim();
        // The render stage is expensive (ROAR init) and keeps the limiter/look-ahead state, so
        // build it once per (source, target) pair and reuse it across loop iterations.
        if render_stage
            .as_ref()
            .is_some_and(|stage| stage.matches(source_layout_id, target))
        {
            let channels = render_stage.as_ref().map_or(0, |stage| stage.channels_out);
            return Ok(rendered_layout_plan(target, source_layout_id, channels));
        }
        *render_stage = None;
        match PcmRenderStage::new(source_layout_id, target, cfg.limiter_db, cfg.gain_db) {
            Ok(stage) => {
                let channels = stage.channels_out;
                *render_stage = Some(stage);
                return Ok(rendered_layout_plan(target, source_layout_id, channels));
            }
            Err(err) => {
                if !*warned {
                    *warned = true;
                    logln!(
                        "WARN PCM ROAR render unavailable ({source_layout_id} -> {target}): {err}; falling back to strict layout intersection"
                    );
                }
            }
        }
    } else {
        *render_stage = None;
    }
    plan_layout(&cfg.layout, source_channels, meta_layout)
}

/// Wire layout when PCM goes through ROAR: the daemon submits target-layout PCM, so the session's
/// input channel count and the SDP labels are the target layout (identity map).
fn rendered_layout_plan(target: &str, source_layout: &str, channels: usize) -> LayoutPlan {
    let labels: Vec<String> = crate::config::layout_labels(target)
        .map(|labels| labels.iter().map(|label| (*label).to_string()).collect())
        .unwrap_or_default();
    LayoutPlan {
        target: target.to_string(),
        source: source_layout.to_string(),
        output_layout: target.to_string(),
        source_channels: channels,
        channels,
        labels,
        map: (0..channels).collect(),
        missing: Vec::new(),
        dropped: Vec::new(),
    }
}

fn make_resampler(
    cfg: &Config,
    rate: u32,
    channels: usize,
) -> Result<Box<dyn Resampler>, String> {
    if rate == crate::OUT_RATE {
        Ok(Box::new(Passthrough))
    } else {
        match cfg.resampler {
            crate::config::ResamplerKind::Rubato => {
                Ok(Box::new(RubatoResampler::new(channels)?))
            }
            crate::config::ResamplerKind::CPoly => Ok(Box::new(PolyphaseResampler::new())),
        }
    }
}

pub fn plan_flows(
    cfg: &Config,
    timing: &PacketTiming,
    encoding: RtpEncoding,
    channels: usize,
    now_ns: i64,
) -> Result<Vec<FlowPlan>, String> {
    let samples = timing.samples_per_packet() as usize;
    let payload_per_channel = samples * encoding.bytes();
    if payload_per_channel == 0 || payload_per_channel > MAX_RTP_PAYLOAD {
        return Err(format!(
            "{samples} samples x {} bytes exceeds the 1440-byte AES67 payload budget",
            encoding.bytes()
        ));
    }
    let payload_cap = (MAX_RTP_PAYLOAD / payload_per_channel).max(1);
    let groups: Vec<(usize, usize)> = if cfg.legacy_split {
        // C v5.3 A/B reference mode: one flow with every channel, RTP
        // fragmentation at --mtu (>MTU payloads split on frame boundaries).
        let payload = channels * payload_per_channel;
        if cfg.no_split && (12 + payload > cfg.mtu || payload > MAX_RTP_PAYLOAD) {
            return Err(format!(
                "--legacy-split --no-split: {channels} channels x {payload_per_channel}B = {payload}B payload does not fit mtu {} / the 1440B AES67 ceiling",
                cfg.mtu
            ));
        }
        vec![(0, channels)]
    } else if cfg.no_split {
        let cap = STANDARD_FLOW_CHANNELS.max(MAX_FLOW_CHANNELS).min(payload_cap);
        if channels > cap {
            return Err(format!(
                "--no-split: {channels} channels x {payload_per_channel}B do not fit the 1440-byte AES67 payload budget (max {cap} channels in one flow); \
use standard multi-flow packing (drop --no-split) instead"
            ));
        }
        vec![(0, channels)]
    } else {
        let per_flow = payload_cap.min(STANDARD_FLOW_CHANNELS).max(1);
        let mut groups = Vec::new();
        let mut offset = 0;
        while offset < channels {
            let count = per_flow.min(channels - offset);
            groups.push((offset, count));
            offset += count;
        }
        groups
    };
    let base_ssrc = cfg
        .ssrc
        .unwrap_or_else(|| 0x4E53_5034 ^ (now_ns as u32));
    groups
        .iter()
        .enumerate()
        .map(|(index, &(offset, count))| {
            let port = cfg
                .port
                .checked_add(2 * index as u16)
                .ok_or_else(|| format!("flow {index} port overflow"))?;
            Ok(FlowPlan {
                index,
                offset,
                channels: count,
                dest: SocketAddrV4::new(cfg.group_ip(), port),
                ssrc: base_ssrc.wrapping_add(index as u32),
                payload_type: cfg.payload_type(),
                fragment_mtu: if cfg.legacy_split && !cfg.no_split {
                    cfg.mtu
                } else {
                    0
                },
            })
        })
        .collect()
}

/// Build the vendor session config for one resolved layout.
///
/// `layout.source_channels` is the producer/FIFO width, `layout.channels` is the
/// actual output width after the target/source intersection; the vendor ring
/// map selects the output channels from each source frame. SDP and status use
/// this actual layout.
/// Start a session, implementing the TV-facing PTP policy:
///   ptp=0                -> local clock
///   ptp=1, ptp_gm empty  -> multicast PTP (default GM)
///   ptp=1, ptp_gm=<IP>   -> try unicast first, fall back to multicast when the
///                           GM does not answer within the session lock timeout.
/// Returns the effective mode for status.json.
fn start_session_with_ptp_fallback(
    cfg: &Config,
    local_ip: Ipv4Addr,
    timing: &PacketTiming,
    encoding: RtpEncoding,
    layout: &LayoutPlan,
    plan: &[FlowPlan],
    hint: &str,
) -> Result<(Session, &'static str), String> {
    let _ = crate::LOCAL_IP.get_or_init(|| local_ip);
    let attempt = |master: Option<Ipv4Addr>| -> Result<Session, String> {
        let mut candidate = cfg.clone();
        candidate.ptp_master = master;
        let config = build_session_config_for(
            &candidate,
            local_ip,
            timing,
            encoding,
            layout,
            plan,
        )?;
        let session = Session::default();
        let rc = session.configure(config);
        if rc != 0 {
            return Err(format!("session configure failed (code {rc})"));
        }
        let rc = session.start();
        if rc != 0 {
            return Err(format!(
                "session start failed (code {rc}); no PTP lock within the lock timeout"
            ));
        }
        Ok(session)
    };
    if !cfg.ptp {
        return attempt(None).map(|session| (session, "local"));
    }
    let Some(master) = cfg.ptp_master else {
        return attempt(None).map(|session| (session, "multicast"));
    };
    // The unicast grandmaster can be unreachable (no reply) and each attempt costs
    // the full lock timeout. Once multicast was the winning mode in this process,
    // start there directly: source switches then no longer pay that timeout.
    if hint == "multicast" {
        match attempt(None) {
            Ok(session) => return Ok((session, "multicast")),
            Err(err) => logln!(
                "WARN PTP multicast GM did not lock ({}); retrying unicast master {}",
                err,
                master
            ),
        }
    }
    match attempt(Some(master)) {
        Ok(session) => Ok((session, "unicast")),
        Err(err) => {
            logln!(
                "WARN PTP unicast master {} did not lock ({}); falling back to multicast GM",
                master,
                err
            );
            attempt(None)
                .map(|session| (session, "multicast"))
                .map_err(|fallback| {
                    format!("{err}; multicast fallback also failed: {fallback}")
                })
        }
    }
}

pub fn build_session_config_for(
    cfg: &Config,
    local_ip: Ipv4Addr,
    timing: &PacketTiming,
    encoding: RtpEncoding,
    layout: &LayoutPlan,
    plan: &[FlowPlan],
) -> Result<SessionConfig, String> {
    let source_channels = layout.source_channels;
    let channels = layout.channels;
    let mut map = layout.map.clone();
    if !cfg.channel_map.is_empty() {
        if cfg.channel_map.len() != channels {
            return Err(format!(
                "channel_map has {} entries but the actual layout has {channels} channels",
                cfg.channel_map.len()
            ));
        }
        for (index, source) in cfg.channel_map.iter().enumerate() {
            if *source >= source_channels {
                return Err(format!(
                    "channel_map[{index}]={source} out of range for {source_channels} source channels"
                ));
            }
        }
        map = cfg.channel_map.clone();
    }
    // Labels: the intersection's labels are the truth; explicit daemon.conf
    // labels may override individual positions (used by the legacy C contract).
    let mut labels = layout.labels.clone();
    for (index, label) in cfg.labels.iter().enumerate().take(labels.len()) {
        if !label.is_empty() {
            labels[index] = label.clone();
        }
    }
    let ptp = ptp::Config {
        domain: cfg.ptp_domain,
        pinned_gm: None,
        interface: cfg.ptp_interface.unwrap_or(local_ip),
        multicast: Ipv4Addr::new(224, 0, 1, 129),
        unicast_master: cfg.ptp_master,
        event_port: 319,
        general_port: 320,
        local_event_port: 0,
        local_general_port: 0,
    };
    let flows: Vec<Flow> = plan
        .iter()
        .map(|flow| Flow {
            destination: flow.dest,
            interface: local_ip,
            ttl: 32,
            ssrc: flow.ssrc,
            offset: flow.offset,
            channels: flow.channels,
            payload_type: flow.payload_type,
            source_filter: cfg.source_filter,
            fragment_mtu: flow.fragment_mtu,
        })
        .collect();
    Ok(SessionConfig {
        channels,
        inputs: source_channels,
        map,
        labels,
        name: "AES67Bridge HAL tee (rust)".to_string(),
        capacity: cfg.buffer_ms * crate::OUT_RATE as usize / 1000,
        prebuffer: cfg.prebuffer_ms * crate::OUT_RATE as usize / 1000,
        ptp,
        mode: TimestampMode::Software,
        lock_timeout: Duration::from_millis(if cfg.ptp_master.is_some() {
            // TV policy: try the configured unicast GM briefly, then fall back
            // to multicast so a non-negotiating GM does not delay movie start.
            2_000
        } else {
            8_000
        }),
        flows,
        sap: cfg.sap,
        sap_interval: Duration::from_secs(u64::from(cfg.sap_interval_s.max(1))),
        sap_destination: cfg
            .sap_destination
            .unwrap_or(Ipv4Addr::new(224, 2, 127, 254)),
        timing: *timing,
        encoding,
        redundant: false,
        local_clock: !cfg.ptp,
    })
}

/// Legacy helper for callers that already know the exact channel count and do
/// not need target/source intersection (compliance matrix, A/B tools). When a
/// target layout is configured it is used as both target and source, which is
/// the identity case.
pub fn build_session_config(
    cfg: &Config,
    local_ip: Ipv4Addr,
    timing: &PacketTiming,
    encoding: RtpEncoding,
    channels: usize,
    plan: &[FlowPlan],
) -> Result<SessionConfig, String> {
    let source_layout = if cfg.layout.trim().is_empty() {
        None
    } else {
        Some(cfg.layout.as_str())
    };
    let layout = plan_layout(&cfg.layout, channels, source_layout)?;
    build_session_config_for(cfg, local_ip, timing, encoding, &layout, plan)
}

fn publish_sdp(session: &Session, cfg: &Config, plan: &[FlowPlan], shared: &SharedState) {
    let mut sdps = Vec::new();
    for index in 0..plan.len() {
        match session.sdp_text(index) {
            Some(sdp) => sdps.push(sdp),
            None => {
                logln!("WARN SDP for flow {index} unavailable");
                return;
            }
        }
    }
    for (index, sdp) in sdps.iter().enumerate() {
        let path = if index == 0 {
            cfg.sdp.clone()
        } else {
            format!("{}.{}", cfg.sdp, index)
        };
        match write_atomic(&path, sdp.as_bytes()) {
            Ok(()) => logln!(
                "SDP written {} ch={} flow={} transport={}:{}",
                path,
                plan[index].channels,
                index,
                plan[index].dest.ip(),
                plan[index].dest.port()
            ),
            Err(err) => logln!("WARN SDP write {path} failed: {err}"),
        }
    }
    let params = RtpParams {
        sample_rate: crate::OUT_RATE,
        channels: plan.iter().map(|flow| flow.channels).sum(),
        encoding: if cfg.encoding == Encoding::L24 {
            "L24"
        } else {
            "L16"
        },
        ptime_us: cfg.ptime_us,
        split: !cfg.no_split,
        mtu: cfg.mtu,
        flows: plan
            .iter()
            .map(|flow| FlowParams {
                index: flow.index,
                offset: flow.offset,
                channels: flow.channels,
                group: *flow.dest.ip(),
                port: flow.dest.port(),
                ssrc: flow.ssrc,
                payload_type: flow.payload_type,
            })
            .collect(),
    };
    shared.publish(sdps, params);
}

fn write_atomic(path: &str, body: &[u8]) -> io::Result<()> {
    let tmp = format!("{path}.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(body)?;
    }
    platform::chmod_644(&tmp);
    std::fs::rename(&tmp, path)?;
    platform::chmod_644(path);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Write a heartbeat status while the daemon is waiting for a stream (no session
/// yet) or between TrueHD blocks. The TV app treats a missing/stale status file as
/// "daemon not running", so the file must refresh at >=1 Hz even when idle.
/// Minimal SDP pull endpoint. SAP is the AES67 discovery path, but many
/// controllers (and aes67-daemon's web UI) can also ingest an SDP that is fetched
/// over HTTP; serving http://<bridge>:<port>/aes67.sdp keeps the description
/// available even to receivers that do not browse SAP.
fn spawn_sdp_http_server(port: u16, sdp_path: String, local_ip: Ipv4Addr) {
    let spawned = std::thread::Builder::new()
        .name("sdp-http".to_string())
        .spawn(move || {
            use std::io::{Read as _, Write as _};
            let listener = match std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
                Ok(listener) => listener,
                Err(err) => {
                    logln!("WARN SDP HTTP bind :{port} failed: {err}");
                    return;
                }
            };
            logln!("SDP HTTP server: http://{local_ip}:{port}/aes67.sdp");
            for connection in listener.incoming() {
                let Ok(mut stream) = connection else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0_u8; 1024];
                let read = stream.read(&mut request).unwrap_or(0);
                let head = String::from_utf8_lossy(&request[..read]);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let wants_sdp = path.to_ascii_lowercase().ends_with(".sdp");
                let (content_type, body) = if wants_sdp {
                    match std::fs::read_to_string(&sdp_path) {
                        Ok(text) => ("application/sdp", text),
                        Err(err) => ("text/plain", format!("SDP unavailable: {err}\n")),
                    }
                } else {
                    (
                        "text/plain",
                        format!("AES67Bridge SDP server\nGET /aes67.sdp\n"),
                    )
                };
                let response = format!(
                    "HTTP/1.0 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
    if let Err(err) = spawned {
        logln!("WARN SDP HTTP thread spawn failed: {err}");
    }
}

/// Codec-router state (codec=auto): which HAL tee source is live right now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AutoSource {
    Bits,
    Pcm,
    Idle,
}

/// How long a session is kept after its live source goes quiet. AES67 streams are
/// continuous, so a pause keeps sending silence; only a longer gap releases the

fn file_fresh_ms(path: &str, max_ms: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age.as_millis() as u64 <= max_ms)
}

/// live.bits.meta is being refreshed and describes a decodable 48 kHz bitstream.
fn bits_stream_active(cfg: &Config) -> bool {
    if !file_fresh_ms(&cfg.bits_meta, 2_000) {
        return false;
    }
    match meta::parse_bits_meta(&cfg.bits_meta) {
        Some(bits) => {
            !bits.is_stale() && (bits.is_truehd() || (bits.is_dolby_es() && bits.rate == 48_000))
        }
        None => false,
    }
}

/// live.meta is being refreshed and describes a 44.1/48 kHz PCM stream.
fn pcm_stream_active(cfg: &Config) -> bool {
    if !file_fresh_ms(&cfg.meta, 2_000) {
        return false;
    }
    let mut state = MetaState::default();
    meta::parse_meta(&cfg.meta, &mut state);
    state.valid && (state.rate == 44_100 || state.rate == 48_000)
}

fn source_idle(last_data_ns: i64, auto_idle_ms: u64) -> bool {
    auto_idle_ms > 0 && age_ms(last_data_ns) >= auto_idle_ms
}

fn auto_source(cfg: &Config) -> AutoSource {
    if bits_stream_active(cfg) {
        AutoSource::Bits
    } else if pcm_stream_active(cfg) {
        AutoSource::Pcm
    } else {
        AutoSource::Idle
    }
}

fn write_idle_status(cfg: &Config, ptp_mode: &str) {
    let layout = if cfg.layout.trim().is_empty() {
        cfg.effective_layout()
    } else {
        cfg.layout.clone()
    };
    let channels = crate::config::layout_channels(&layout)
        .unwrap_or(cfg.maxch)
        .clamp(2, MAX_CH);
    let timing = match PacketTiming::new(crate::OUT_RATE, cfg.ptime_us) {
        Some(timing) => timing,
        None => return,
    };
    let encoding = if cfg.encoding == Encoding::L24 {
        RtpEncoding::L24
    } else {
        RtpEncoding::L16
    };
    let plan = plan_flows(cfg, &timing, encoding, channels, platform::now_ns()).unwrap_or_default();
    let flow_status: Vec<status::FlowStatus> = plan
        .iter()
        .map(|flow| status::FlowStatus {
            index: flow.index,
            offset: flow.offset,
            channels: flow.channels,
            dest: format!("{}:{}", flow.dest.ip(), flow.dest.port()),
            ssrc: flow.ssrc,
            payload_type: flow.payload_type,
        })
        .collect();
    let first = plan.first();
    let levels = vec![-120.0f64; channels];
    let include_bits = matches!(cfg.codec.as_str(), "bits" | "auto" | "truehd");
    let bits_view = status::BitsStatus {
        object_count: 0,
        decode_render_ms_p50: 0.0,
        decode_render_ms_p99: 0.0,
        decode_errors: 0,
        extract_errors: 0,
        limited: 0,
        out_peak: 0.0,
        dialnorm_db: 0.0,
        _marker: std::marker::PhantomData,
    };
    let snapshot = status::Snapshot {
        layout: &layout,
        mode: "normal",
        group: &cfg.group,
        port: cfg.port,
        maxch: channels,
        codec: &cfg.codec,
        stream_id: 0,
        fmt_name: "PCM_16",
        rate: crate::OUT_RATE,
        channels,
        packets: 0,
        send_err: 0,
        bytes_sent: 0,
        silent: 0,
        late: 0,
        dropped: 0,
        ssrc: first.map_or(0, |flow| flow.ssrc),
        payload_type: cfg.payload_type(),
        dest: first.map_or_else(String::new, |flow| {
            format!("{}:{}", flow.dest.ip(), flow.dest.port())
        }),
        encoding: if cfg.encoding == Encoding::L24 {
            "L24"
        } else {
            "L16"
        },
        flow_count: plan.len(),
        split_mode: if cfg.legacy_split || cfg.no_split {
            "legacy"
        } else {
            "standard"
        },
        flows: &flow_status,
        rs_in: 0,
        rs_out: 0,
        src_active: false,
        src_age_ms: 0,
        src_codec: "none",
        src_rate: crate::OUT_RATE,
        levels: &levels,
        ptime_us: cfg.ptime_us,
        split: !cfg.no_split,
        ring_fill: 0,
        fifo_bytes: 0,
        locked: false,
        timestamping: 0,
        offset_ns: 0.0,
        rate_ppm: 0.0,
        fit_rms_ns: 0.0,
        ptp_mode,
        sap: cfg.sap,
        sap_interval_s: cfg.sap_interval_s,
        bits: if include_bits { Some(&bits_view) } else { None },
    };
    status::publish(&snapshot);
}

fn write_status(
    session: &Session,
    cfg: &Config,
    plan: &[FlowPlan],
    parts: &StatusParts<'_>,
    fifo_last: usize,
    shape: &StreamShape,
    ptp_mode: &str,
    bits: Option<&status::BitsStatus<'_>>,
) {
    let counters = session.counters();
    let flow_status: Vec<FlowStatus> = plan
        .iter()
        .map(|flow| FlowStatus {
            index: flow.index,
            offset: flow.offset,
            channels: flow.channels,
            dest: format!("{}:{}", flow.dest.ip(), flow.dest.port()),
            ssrc: flow.ssrc,
            payload_type: flow.payload_type,
        })
        .collect();
    let first = plan.first();
    // SDP/status always report the adapter (intersection) result, never the
    // declared target layout.
    let layout = if shape.layout.is_empty() {
        cfg.effective_layout()
    } else {
        shape.layout.clone()
    };
    // Level meters are shown in output-layout order (what the user sees), not
    // in raw source order.
    let mut levels = vec![-120.0f64; shape.map.len()];
    for (out, &source) in shape.map.iter().enumerate() {
        if source < MAX_CH {
            levels[out] = parts.levels[source];
        }
    }
    let snapshot = Snapshot {
        layout: &layout,
        mode: if std::path::Path::new("/data/haltee/latency.frames").exists() {
            "lowlat"
        } else {
            "normal"
        },
        group: &cfg.group,
        port: cfg.port,
        maxch: shape.channels,
        codec: &cfg.codec,
        stream_id: shape.stream_id,
        fmt_name: shape.fmt.name(),
        rate: shape.rate,
        channels: plan.iter().map(|flow| flow.channels).sum(),
        packets: counters.packets,
        send_err: counters.errors,
        bytes_sent: counters.bytes,
        silent: counters.silent,
        late: counters.late,
        dropped: counters.dropped,
        ssrc: first.map_or(0, |flow| flow.ssrc),
        payload_type: cfg.payload_type(),
        dest: first.map_or_else(String::new, |flow| {
            format!("{}:{}", flow.dest.ip(), flow.dest.port())
        }),
        encoding: if cfg.encoding == Encoding::L24 {
            "L24"
        } else {
            "L16"
        },
        flow_count: plan.len(),
        split_mode: if cfg.legacy_split || cfg.no_split {
            "legacy"
        } else {
            "standard"
        },
        flows: &flow_status,
        rs_in: parts.rs_in,
        rs_out: parts.rs_out,
        src_active: parts.src_active,
        src_age_ms: parts.src_age_ms,
        src_codec: parts.src_codec,
        src_rate: parts.src_rate,
        levels: &levels,
        ptime_us: cfg.ptime_us,
        split: !cfg.no_split,
        ring_fill: session.ring_frames(),
        fifo_bytes: fifo_last,
        locked: counters.locked != 0,
        timestamping: counters.timestamping,
        offset_ns: counters.offset_ns,
        rate_ppm: counters.rate_ppm,
        fit_rms_ns: counters.fit_rms_ns,
        ptp_mode,
        sap: cfg.sap,
        sap_interval_s: cfg.sap_interval_s,
        bits,
    };
    status::publish(&snapshot);
    let _ = parts.submitted;}
#[cfg(test)]
mod pcm_render_tests {
    use super::*;

    fn tone(frames: usize, freq: f32, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                let w = 2.0 * std::f32::consts::PI * freq * index as f32 / crate::OUT_RATE as f32;
                amplitude * w.sin()
            })
            .collect()
    }

    fn channel_energy(pending: &[f32], channels: usize) -> Vec<f32> {
        let mut sums = vec![0.0f32; channels];
        for frame in pending.chunks_exact(channels) {
            for (channel, &sample) in frame.iter().enumerate() {
                sums[channel] += sample * sample;
            }
        }
        sums
    }

    #[test]
    fn pcm_render_stage_512_to_512_keeps_every_channel_in_place() {
        // 5.1.2 -> 5.1.2 must be identity: L/R/C/LFE/Ls/Rs and both height
        // channels stay where they are. Each channel gets a distinct level, so a
        // mistaken 5.1.2->7.1 read (heights into Lb/Rb) or any folding would show
        // up as a channel-energy mismatch.
        let mut stage = PcmRenderStage::new("5.1.2", "5.1.2", -1.0, 0.0).expect("stage");
        assert_eq!(stage.channels_in, 8);
        assert_eq!(stage.channels_out, 8);
        let amps = [0.05f32, 0.075, 0.10, 0.125, 0.15, 0.175, 0.20, 0.225];
        let mut meter = LevelMeter::default();
        let mut submitter = Submitter::new(8);
        let mut frame = [0_i32; MAX_CH];
        for index in 0..480usize {
            frame = [0_i32; MAX_CH];
            for (channel, &amp) in amps.iter().enumerate() {
                let w = 2.0 * std::f32::consts::PI
                    * (300.0 + 100.0 * channel as f32)
                    * index as f32
                    / crate::OUT_RATE as f32;
                frame[channel] = (amp * w.sin() * 8_388_608.0) as i32;
            }
            stage.push(&frame, 8, &mut meter, &mut submitter);
        }
        assert_eq!(submitter.frames, 480);
        let energy = channel_energy(&submitter.pending, 8);
        // Relative energy per channel must match amp^2: no channel moved, no fold.
        let base = energy[0] / (amps[0] * amps[0]);
        for (channel, &amp) in amps.iter().enumerate() {
            let ratio = energy[channel] / (amp * amp);
            assert!(
                (ratio - base).abs() < base * 0.05,
                "channel {channel} energy ratio {ratio} vs base {base}"
            );
        }
    }

    #[test]
    fn pcm_render_stage_folds_712_into_512() {
        let mut stage = PcmRenderStage::new("7.1.2", "5.1.2", -1.0, 0.0).expect("stage");
        assert_eq!(stage.channels_in, 10);
        assert_eq!(stage.channels_out, 8);
        let mut meter = LevelMeter::default();
        let mut submitter = Submitter::new(8);
        // 7.1.2 order: L R C LFE Ls Rs Lb Rb Tfl Tfr.
        let lfe = tone(480, 60.0, 0.1);
        let back = tone(480, 700.0, 0.5);
        let top = tone(480, 900.0, 0.4);
        let mut frame = [0_i32; MAX_CH];
        for index in 0..480 {
            frame = [0_i32; MAX_CH];
            frame[3] = (lfe[index] * 8_388_608.0) as i32; // LFE
            frame[6] = (back[index] * 8_388_608.0) as i32; // Lb
            frame[7] = (back[index] * 8_388_608.0) as i32; // Rb
            frame[8] = (top[index] * 8_388_608.0) as i32; // Tfl
            frame[9] = (top[index] * 8_388_608.0) as i32; // Tfr
            stage.push(&frame, 10, &mut meter, &mut submitter);
        }
        assert_eq!(submitter.frames, 480);
        assert_eq!(submitter.pending.len(), 480 * 8);
        let energy = channel_energy(&submitter.pending, 8);
        // 5.1.2 order: L R C LFE Ls Rs Tfl Tfr.
        assert!(energy[3] > 0.1, "LFE did not pass through: {}", energy[3]);
        assert!(energy[4] > 0.1, "Lb did not fold into Ls: {}", energy[4]);
        assert!(energy[5] > 0.1, "Rb did not fold into Rs: {}", energy[5]);
        assert!(energy[6] > 0.05, "Tfl missing: {}", energy[6]);
        assert!(energy[7] > 0.05, "Tfr missing: {}", energy[7]);
        assert_eq!(stage.rendered_blocks, 1);
        assert!(stage.peak > 0.0);
    }

    #[test]
    fn pcm_render_stage_same_layout_passes_channels_through() {
        let mut stage = PcmRenderStage::new("5.1", "5.1", -1.0, 0.0).expect("stage");
        let mut meter = LevelMeter::default();
        let mut submitter = Submitter::new(6);
        let levels = [0.1f32, 0.2, 0.3, 0.05, 0.15, 0.25];
        let mut frame = [0_i32; MAX_CH];
        // Two blocks: the limiter's 5 ms look-ahead delays the first block's tail, so the second
        // block is where the steady-state level shows up.
        for _ in 0..960 {
            frame = [0_i32; MAX_CH];
            for (channel, level) in levels.iter().enumerate() {
                frame[channel] = (level * 8_388_608.0) as i32;
            }
            stage.push(&frame, 6, &mut meter, &mut submitter);
        }
        assert_eq!(submitter.frames, 960);
        let second = &submitter.pending[480 * 6..];
        let energy = channel_energy(second, 6);
        for (channel, level) in levels.iter().enumerate() {
            let expected = (level * level) * 480.0;
            assert!(
                (energy[channel] - expected).abs() < expected * 0.05 + 1e-3,
                "channel {channel} level changed: got {} expected {}",
                energy[channel],
                expected
            );
        }
    }

    #[test]
    fn pcm_render_stage_unknown_source_layout_is_rejected() {
        assert!(PcmRenderStage::new("4.1.2", "5.1.2", -1.0, 0.0).is_err());
        // 4.1.2 must also be rejected by the classifier and fall back to the legacy plan.
        let cfg = Config::default();
        let mut stage = None;
        let mut warned = false;
        let plan = classify_pcm_layout(&cfg, "4.1.2", 7, None, &mut stage, &mut warned);
        assert!(plan.is_ok());
        assert!(stage.is_none());
    }
}

