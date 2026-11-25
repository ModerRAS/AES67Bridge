//! AES67 compliance / contract checks.
//!
//! Every matrix row and every loopback (send -> receive) requirement lives here
//! as a named check. `cargo test` runs them through `tests/compliance.rs` and
//! the `aes67-compliance` binary prints the same table, so the report, CI and
//! tests share one source of truth.
//!
//! Pure checks validate the configuration/SDP/pacing/protocol layer. Loopback
//! checks start a real `nsp_aes67` session on 127.0.0.1 UDP sockets, submit
//! known PCM, receive the datagrams in a thread and compare the reassembled
//! payload sample by sample.

use std::net::{Ipv4Addr, UdpSocket};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nsp_aes67::rtp::{Encoding as RtpEncoding, Flow, Grid};
use nsp_aes67::sdp;
use nsp_aes67::session::Session;
use nsp_aes67::time::PacketTiming;

use crate::config::{Config, Encoding};
use crate::config::plan_layout;
use crate::daemon::{build_session_config, build_session_config_for, plan_flows, FlowPlan};
use crate::MAX_CH;

pub struct Check {
    pub id: &'static str,
    pub pass: bool,
    pub detail: String,
}

impl Check {
    fn pass(id: &'static str, detail: String) -> Self {
        Check {
            id,
            pass: true,
            detail,
        }
    }
    fn fail(id: &'static str, detail: String) -> Self {
        Check {
            id,
            pass: false,
            detail,
        }
    }
}

fn run(id: &'static str, check: impl FnOnce() -> Result<String, String>) -> Check {
    match std::panic::catch_unwind(AssertUnwindSafe(check)) {
        Ok(Ok(detail)) => Check::pass(id, detail),
        Ok(Err(message)) => Check::fail(id, message),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|value| (*value).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "check panicked".to_string());
            Check::fail(id, message)
        }
    }
}

fn assert_that(condition: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message())
    }
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

static NEXT_PORT: AtomicU16 = AtomicU16::new(5600);

fn ports(count: usize) -> u16 {
    NEXT_PORT.fetch_add((count as u16) * 20, Ordering::SeqCst)
}

fn base_config(port: u16, channels: usize) -> Config {
    let mut cfg = Config::default();
    cfg.group = "127.0.0.1".to_string();
    cfg.port = port;
    cfg.local_ip = Some(Ipv4Addr::LOCALHOST);
    cfg.maxch = channels.max(2).min(crate::MAX_CH);
    cfg.buffer_ms = 500;
    cfg.prebuffer_ms = 5;
    cfg.sap = false;
    cfg.legacy_split = false;
    cfg.no_split = false;
    cfg.channel_map.clear();
    cfg.labels.clear();
    cfg.ptp = false;
    cfg
}

fn rtp_encoding(cfg: &Config) -> RtpEncoding {
    if cfg.encoding == Encoding::L16 {
        RtpEncoding::L16
    } else {
        RtpEncoding::L24
    }
}

fn timing(cfg: &Config) -> PacketTiming {
    PacketTiming::new(crate::OUT_RATE, cfg.ptime_us).expect("ptime")
}

struct Engine {
    session: Session,
    plan: Vec<FlowPlan>,
    channels: usize,
    frames_pkt: usize,
    cfg: Config,
}

impl Engine {
    fn start(cfg: Config, channels: usize) -> Result<Self, String> {
        let timing = timing(&cfg);
        let encoding = rtp_encoding(&cfg);
        let plan = plan_flows(&cfg, &timing, encoding, channels, 0x1234_5678)?;
        let session_config = build_session_config(
            &cfg,
            Ipv4Addr::LOCALHOST,
            &timing,
            encoding,
            channels,
            &plan,
        )?;
        let session = Session::default();
        let rc = session.configure(session_config);
        if rc != 0 {
            return Err(format!("session configure code={rc}"));
        }
        let rc = session.start();
        if rc != 0 {
            return Err(format!("session start code={rc}"));
        }
        Ok(Engine {
            session,
            plan,
            channels,
            frames_pkt: timing.samples_per_packet() as usize,
            cfg,
        })
    }

    fn submit_packets(
        &self,
        frames: &[[i32; MAX_CH]],
        frames_pkt: usize,
    ) -> Result<u64, String> {
        let mut first = 0u64;
        let mut buffer: Vec<f32> = Vec::with_capacity(frames_pkt * self.channels);
        for chunk in frames.chunks(frames_pkt) {
            buffer.clear();
            for frame in chunk {
                for channel in 0..self.channels {
                    buffer.push(frame[channel] as f32 / 8_388_608.0);
                }
            }
            let rc = unsafe {
                self.session
                    .submit(first, buffer.as_ptr(), chunk.len(), self.channels)
            };
            if rc != 0 {
                return Err(format!("submit code={rc}"));
            }
            first += chunk.len() as u64;
        }
        Ok(first)
    }

    fn stop(self) {
        let _ = self.session.stop(2_000);
    }
}

#[derive(Clone, Debug)]
pub struct RtpPacket {
    pub pt: u8,
    pub marker: bool,
    pub seq: u16,
    pub ts: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

struct Recorder {
    stop: Arc<AtomicBool>,
    join: JoinHandle<Vec<RtpPacket>>,
}

fn start_recorder(port: u16) -> Result<Recorder, String> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|err| format!("bind recorder {port}: {err}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(40)))
        .map_err(|err| format!("recorder timeout: {err}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let join = thread::spawn(move || {
        let mut packets = Vec::new();
        let mut buffer = [0u8; 65_536];
        loop {
            match socket.recv(&mut buffer) {
                Ok(size) if size >= 12 => packets.push(RtpPacket {
                    pt: buffer[1] & 0x7f,
                    marker: buffer[1] & 0x80 != 0,
                    seq: u16::from_be_bytes([buffer[2], buffer[3]]),
                    ts: u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]),
                    ssrc: u32::from_be_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]),
                    payload: buffer[12..size].to_vec(),
                }),
                Ok(_) => {}
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if stop2.load(Ordering::Acquire) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        packets
    });
    Ok(Recorder { stop, join })
}

impl Recorder {
    fn finish(self) -> Vec<RtpPacket> {
        self.stop.store(true, Ordering::Release);
        self.join.join().unwrap_or_default()
    }
}

fn trim_leading_zero_packets(packets: &[RtpPacket]) -> usize {
    packets
        .iter()
        .position(|packet| packet.payload.iter().any(|&byte| byte != 0))
        .unwrap_or(packets.len())
}

fn sine_frames(count: usize, channels: usize, rate: u32, hz: f64) -> Vec<[i32; MAX_CH]> {
    (0..count)
        .map(|n| {
            let mut frame = [0i32; MAX_CH];
            for (c, item) in frame.iter_mut().enumerate().take(channels) {
                let phase = std::f64::consts::TAU * hz * (c as f64 + 1.0) * n as f64 / rate as f64;
                *item = (phase.sin() * 4_000_000.0) as i32;
            }
            frame
        })
        .collect()
}

fn expected_payload(
    frames: &[[i32; MAX_CH]],
    channels: usize,
    offset: usize,
    flow_channels: usize,
    encoding: RtpEncoding,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames.len() * flow_channels * encoding.bytes());
    for frame in frames {
        for channel in offset..offset + flow_channels.min(channels - offset) {
            let value = frame[channel] & 0x00ff_ffff;
            match encoding {
                RtpEncoding::L24 => {
                    out.push(((value >> 16) & 0xff) as u8);
                    out.push(((value >> 8) & 0xff) as u8);
                    out.push((value & 0xff) as u8);
                }
                RtpEncoding::L16 => {
                    // Encoding::pack rounds sample*32768 half away from zero,
                    // i.e. round(i24 / 256); reproduce it exactly.
                    let signed = if value & 0x0080_0000 != 0 {
                        value | !0x00ff_ffff
                    } else {
                        value
                    };
                    let half = if signed >= 0 { 128 } else { -128 };
                    let rounded = (signed + half) / 256;
                    let word = (rounded as i16).to_be_bytes();
                    out.push(word[0]);
                    out.push(word[1]);
                }
            }
        }
    }
    out
}

/// Run one send/receive case and return (received packets, expected payload).
fn loopback_case(
    cfg: Config,
    channels: usize,
    frames: &[[i32; MAX_CH]],
    settle: Duration,
) -> Result<(Vec<RtpPacket>, Vec<u8>, Vec<FlowPlan>, RtpEncoding), String> {
    let encoding = rtp_encoding(&cfg);
    let engine = Engine::start(cfg.clone(), channels)?;
    let plan = engine.plan.clone();
    let frames_pkt = engine.frames_pkt;
    let mut recorders = Vec::new();
    for flow in &plan {
        recorders.push(start_recorder(flow.dest.port())?);
    }
    engine.submit_packets(frames, frames_pkt)?;
    thread::sleep(settle);
    if std::env::var_os("AES67_TRACE").is_some() {
        let c = engine.session.counters();
        eprintln!(
            "loopback ptime={}us ch={} frames_pkt={} submitted={} counters packets={} silent={} late={} dropped={} errors={} locked={} ring={}",
            cfg.ptime_us, channels, frames_pkt, frames.len(),
            c.packets, c.silent, c.late, c.dropped, c.errors, c.locked,
            engine.session.ring_frames()
        );
    }
    engine.stop();
    thread::sleep(Duration::from_millis(80));
    let mut all = Vec::new();
    for recorder in recorders {
        all.push(recorder.finish());
    }
    // Multi-flow callers read the remaining recordings through the plan.
    let expected = expected_payload(frames, channels, plan[0].offset, plan[0].channels, encoding);
    Ok((all.remove(0), expected, plan, encoding))
}

fn check_seq_ts(packets: &[RtpPacket], frames_pkt: usize) -> Result<(), String> {
    for pair in packets.windows(2) {
        let expected_seq = pair[0].seq.wrapping_add(1);
        assert_that(pair[1].seq == expected_seq, || {
            format!(
                "sequence gap: {} -> {}",
                pair[0].seq, pair[1].seq
            )
        })?;
        let expected_ts = pair[0].ts.wrapping_add(frames_pkt as u32);
        assert_that(pair[1].ts == expected_ts, || {
            format!("timestamp step: {} -> {}", pair[0].ts, pair[1].ts)
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// checks
// ---------------------------------------------------------------------------

fn check_ptime_frames() -> Result<String, String> {
    let mut detail = Vec::new();
    for ptime in [125u32, 250, 500, 1_000] {
        let timing = PacketTiming::new(crate::OUT_RATE, ptime).expect("ptime");
        let frames = timing.samples_per_packet() as u64;
        let expected = u64::from(crate::OUT_RATE) * u64::from(ptime) / 1_000_000;
        assert_that(frames == expected, || {
            format!("ptime {ptime}us: {frames} frames != {expected}")
        })?;
        let l24 = frames as usize * 3;
        let l16 = frames as usize * 2;
        detail.push(format!("{ptime}us:{frames}f L24={l24}B L16={l16}B"));
    }
    Ok(detail.join(" "))
}

fn check_encoding_pt() -> Result<String, String> {
    let l24 = Encoding::L24;
    let l16 = Encoding::L16;
    assert_that(l24.default_pt() == 97, || "L24 PT".to_string())?;
    assert_that(l16.default_pt() == 96, || "L16 PT".to_string())?;
    assert_that(RtpEncoding::L24.bytes() == 3, || "L24 width".to_string())?;
    assert_that(RtpEncoding::L16.bytes() == 2, || "L16 width".to_string())?;
    Ok("L24=PT97/3B L16=PT96/2B".to_string())
}

fn check_standard_flow_packing() -> Result<String, String> {
    let cfg = base_config(5000, 16);
    let timing = timing(&cfg);
    for (channels, expected) in [(2usize, 1usize), (8, 1), (10, 2), (12, 2), (16, 2)] {
        let plan = plan_flows(&cfg, &timing, RtpEncoding::L24, channels, 1)?;
        assert_that(plan.len() == expected, || {
            format!("{channels}ch -> {} flows (expected {expected})", plan.len())
        })?;
        assert_that(plan.iter().all(|flow| flow.channels <= 8), || {
            "flow > 8 channels".to_string()
        })?;
        let covered: usize = plan.iter().map(|flow| flow.channels).sum();
        assert_that(covered == channels, || "channel coverage".to_string())?;
    }
    Ok("2/8ch -> 1 flow, 10/12/16ch -> 2 flows of <=8ch".to_string())
}

fn check_flow_identity() -> Result<String, String> {
    let cfg = base_config(5000, 16);
    let timing = timing(&cfg);
    let plan = plan_flows(&cfg, &timing, RtpEncoding::L24, 10, 1)?;
    assert_that(plan[0].offset == 0 && plan[1].offset == 8, || "offsets".to_string())?;
    assert_that(
        plan[1].dest.port() == plan[0].dest.port() + 2,
        || "port step".to_string(),
    )?;
    assert_that(plan[0].ssrc != plan[1].ssrc, || "ssrc unique".to_string())?;
    let labels = (0..10).map(|c| format!("CH{}", c + 1)).collect::<Vec<_>>();
    let session = build_session_config(
        &cfg,
        Ipv4Addr::LOCALHOST,
        &timing,
        RtpEncoding::L24,
        10,
        &plan,
    )?;
    assert_that(session.labels.len() == 10, || "labels length".to_string())?;
    let _ = labels;
    Ok(format!(
        "flows=2 ports={} -> {} ssrcs=0x{:08x}/0x{:08x}",
        plan[0].dest.port(),
        plan[1].dest.port(),
        plan[0].ssrc,
        plan[1].ssrc
    ))
}

fn check_no_split_reject() -> Result<String, String> {
    let mut cfg = base_config(5000, 16);
    cfg.legacy_split = true;
    cfg.no_split = true;
    cfg.mtu = 1500;
    let timing = timing(&cfg);
    let wide = plan_flows(&cfg, &timing, RtpEncoding::L24, 16, 1);
    assert_that(wide.is_err(), || {
        "16ch legacy no-split should be refused".to_string()
    })?;
    let narrow = plan_flows(&cfg, &timing, RtpEncoding::L24, 8, 1);
    assert_that(narrow.is_ok(), || "8ch no-split should fit".to_string())?;
    // Extension mode: 10ch L24 @1ms = 1440B payload fits exactly.
    let mut extension = base_config(5000, 16);
    extension.no_split = true;
    let ext = plan_flows(&extension, &timing, RtpEncoding::L24, 10, 1)?;
    assert_that(ext.len() == 1 && ext[0].channels == 10, || "extension flow".to_string())?;
    let too_wide = plan_flows(&extension, &timing, RtpEncoding::L24, 12, 1);
    assert_that(too_wide.is_err(), || "12ch extension should be refused".to_string())?;
    Ok("legacy no-split 16ch refused; extension 10ch ok, 12ch refused".to_string())
}

fn check_channel_map_labels() -> Result<String, String> {
    let mut cfg = base_config(5000, 8);
    cfg.channel_map = vec![1, 0, 2, 3];
    cfg.labels = vec!["L".to_string(), "R".to_string()];
    let timing = timing(&cfg);
    let plan = plan_flows(&cfg, &timing, RtpEncoding::L24, 4, 1)?;
    let session = build_session_config(
        &cfg,
        Ipv4Addr::LOCALHOST,
        &timing,
        RtpEncoding::L24,
        4,
        &plan,
    )?;
    assert_that(session.map == vec![1, 0, 2, 3], || {
        format!("map {:?}", session.map)
    })?;
    assert_that(
        session.labels == vec!["L", "R", "CH3", "CH4"],
        || format!("labels {:?}", session.labels),
    )?;
    Ok("map=[1,0,2,3] labels=[L,R,CH3,CH4]".to_string())
}

fn sdp_output(port: u16, channels: usize, encoding: RtpEncoding, filter: bool) -> Flow {
    Flow {
        destination: std::net::SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        interface: Ipv4Addr::LOCALHOST,
        ttl: 32,
        ssrc: 0x4e53_0001,
        offset: 0,
        channels,
        payload_type: if encoding == RtpEncoding::L24 { 97 } else { 96 },
        source_filter: filter,
        fragment_mtu: 0,
    }
}

fn describe_for(port: u16, channels: usize, encoding: RtpEncoding, version: u64) -> String {
    let output = nsp_aes67::rtp::Output::open(
        sdp_output(port, channels, encoding, false),
        encoding,
    )
    .expect("output");
    let labels: Vec<String> = (0..channels).map(|c| format!("CH{}", c + 1)).collect();
    sdp::describe(
        "AES67Bridge compliance",
        &output,
        0,
        &labels,
        None,
        1,
        version,
        PacketTiming::new(crate::OUT_RATE, 1_000).expect("timing"),
        None,
    )
}

/// UI contract rows: layout name, channel count and canonical speaker labels.
/// Kept independent from config::layout_labels so a typo in either copy fails
/// the matrix instead of silently agreeing.
const UI_LAYOUTS: &[(&str, usize, &[&str])] = &[
    ("2.0", 2, &["L", "R"]),
    ("4.1.2", 7, &["L", "R", "LFE", "Ls", "Rs", "Tfl", "Tfr"]),
    ("5.1", 6, &["L", "R", "C", "LFE", "Ls", "Rs"]),
    ("5.1.2", 8, &["L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr"]),
    ("7.1", 8, &["L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb"]),
    (
        "7.1.2",
        10,
        &["L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr"],
    ),
    (
        "5.1.4",
        10,
        &["L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr", "Tbl", "Tbr"],
    ),
    (
        "7.1.4",
        12,
        &[
            "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr",
        ],
    ),
    (
        "9.1.6",
        16,
        &[
            "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Lw", "Rw", "Tfl", "Tfr", "Tbl",
            "Tbr", "Tsl", "Tsr",
        ],
    ),
];

fn check_layout_matrix() -> Result<String, String> {
    let timing = PacketTiming::new(crate::OUT_RATE, 1_000).expect("timing");
    let mut detail = Vec::new();
    for &(layout, channels, expected_labels) in UI_LAYOUTS {
        let mut cfg = base_config(5000, 8);
        cfg.layout = layout.to_string();
        // Deliberately disagree first: the layout must win and record a note.
        cfg.maxch = 8;
        cfg.resolve_layout();
        assert_that(cfg.maxch == channels, || {
            format!("{layout}: maxch={} (layout implies {channels})", cfg.maxch)
        })?;
        assert_that(cfg.effective_layout() == layout, || {
            format!("{layout}: effective_layout {}", cfg.effective_layout())
        })?;
        assert_that(
            crate::config::layout_channels(layout) == Some(channels),
            || format!("{layout}: helper channel count"),
        )?;
        let expected_flows = if channels <= 8 { 1 } else { 2 };
        let plan = plan_flows(&cfg, &timing, RtpEncoding::L24, channels, 1)?;
        assert_that(plan.len() == expected_flows, || {
            format!("{layout}: {} flows (expected {expected_flows})", plan.len())
        })?;
        assert_that(
            plan.iter().map(|flow| flow.channels).sum::<usize>() == channels,
            || format!("{layout}: channel coverage"),
        )?;
        assert_that(plan.iter().all(|flow| flow.channels <= 8), || {
            format!("{layout}: a flow above 8 channels")
        })?;
        if channels > 8 {
            assert_that(plan[0].channels == 8, || {
                format!("{layout}: first flow must carry 8 channels")
            })?;
            assert_that(
                plan[1].dest.port() == plan[0].dest.port() + 2
                    && plan[1].offset == 8
                    && plan[1].ssrc != plan[0].ssrc,
                || format!("{layout}: second flow identity"),
            )?;
        }
        let session = build_session_config(
            &cfg,
            Ipv4Addr::LOCALHOST,
            &timing,
            RtpEncoding::L24,
            channels,
            &plan,
        )?;
        let want: Vec<String> = expected_labels.iter().map(|s| s.to_string()).collect();
        assert_that(session.labels == want, || {
            format!("{layout}: session labels {:?}", session.labels)
        })?;
        assert_that(session.map == (0..channels).collect::<Vec<_>>(), || {
            format!("{layout}: channel map must be the identity")
        })?;
        // Per-flow SDP: channel count, speaker labels and channel offset.
        for (index, flow) in plan.iter().enumerate() {
            let output = nsp_aes67::rtp::Output::open(
                Flow {
                    destination: flow.dest,
                    interface: Ipv4Addr::LOCALHOST,
                    ttl: 32,
                    ssrc: flow.ssrc,
                    offset: flow.offset,
                    channels: flow.channels,
                    payload_type: 97,
                    source_filter: false,
                    fragment_mtu: 0,
                },
                RtpEncoding::L24,
            )
            .expect("layout flow socket");
            let text = sdp::describe(
                "AES67Bridge layout",
                &output,
                index,
                &session.labels,
                None,
                1,
                1,
                timing,
                None,
            );
            let labels = expected_labels[flow.offset..flow.offset + flow.channels].join(",");
            for wanted in [
                format!("a=rtpmap:97 L24/48000/{}\r\n", flow.channels),
                format!("a=comment:channel-labels={labels}\r\n"),
                format!("a=comment:channel-offset={}\r\n", flow.offset),
            ] {
                assert_that(text.contains(&wanted), || {
                    format!("{layout} flow{index}: missing {wanted:?} in {text}")
                })?;
            }
        }
        detail.push(format!(
            "{layout}={channels}ch/{}flow/{}{}",
            plan.len(),
            expected_labels.join("+"),
            if channels > 8 {
                format!(
                    " [flow0={}ch flow1={}ch]",
                    plan[0].channels, plan[1].channels
                )
            } else {
                String::new()
            }
        ));
    }
    Ok(detail.join(" "))
}

fn check_layout_intersection() -> Result<String, String> {
    let timing = PacketTiming::new(crate::OUT_RATE, 1_000).expect("timing");
    // target layout, source channels, source layout, actual layout, actual SDP labels
    let cases: &[(&str, usize, &str, &str, &[&str])] = &[
        (
            "5.1.2",
            6,
            "5.1",
            "5.1",
            &["L", "R", "C", "LFE", "Ls", "Rs"],
        ),
        (
            "7.1.2",
            8,
            "7.1",
            "7.1",
            &["L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb"],
        ),
        (
            "7.1.2",
            6,
            "5.1",
            "5.1",
            &["L", "R", "C", "LFE", "Ls", "Rs"],
        ),
        (
            "5.1",
            8,
            "5.1.2",
            "5.1",
            &["L", "R", "C", "LFE", "Ls", "Rs"],
        ),
    ];
    let mut detail = Vec::new();
    for &(target, source_channels, source_layout, actual, labels) in cases {
        let plan = plan_layout(target, source_channels, Some(source_layout))?;
        assert_that(plan.output_layout == actual, || {
            format!("{target}+{source_layout}: actual layout {}", plan.output_layout)
        })?;
        assert_that(plan.channels == labels.len(), || {
            format!("{target}+{source_layout}: {} actual channels", plan.channels)
        })?;
        let want: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
        assert_that(plan.labels == want, || {
            format!("{target}+{source_layout}: labels {:?}", plan.labels)
        })?;
        assert_that(plan.map.len() == plan.channels, || "map length".to_string())?;
        let cfg = base_config(5000, plan.channels);
        let flows = plan_flows(&cfg, &timing, RtpEncoding::L24, plan.channels, 1)?;
        let session = build_session_config_for(
            &cfg,
            Ipv4Addr::LOCALHOST,
            &timing,
            RtpEncoding::L24,
            &plan,
            &flows,
        )?;
        assert_that(session.labels == plan.labels, || {
            format!("{target}+{source_layout}: session labels {:?}", session.labels)
        })?;
        // SDP must advertise the actual output layout, never the target.
        let output = nsp_aes67::rtp::Output::open(
            sdp_output(5000, plan.channels, RtpEncoding::L24, false),
            RtpEncoding::L24,
        )
        .expect("intersection output");
        let text = sdp::describe(
            "AES67Bridge intersection",
            &output,
            0,
            &plan.labels,
            None,
            1,
            1,
            timing,
            None,
        );
        let rtpmap = format!("a=rtpmap:97 L24/48000/{}\r\n", plan.channels);
        assert_that(text.contains(&rtpmap), || {
            format!("{target}+{source_layout}: SDP missing {rtpmap:?}")
        })?;
        let label_line = format!("a=comment:channel-labels={}\r\n", plan.labels.join(","));
        assert_that(text.contains(&label_line), || {
            format!("{target}+{source_layout}: SDP missing {label_line:?}")
        })?;
        for missing in plan.missing.iter().filter(|name| name.len() > 1) {
            assert_that(
                !text.contains(&format!(",{missing}")) && !text.contains(&format!("{missing},")),
                || format!("{target}+{source_layout}: SDP invented {missing}"),
            )?;
        }
        detail.push(format!(
            "{target}+{source_layout}={actual}({}ch; missing={}; dropped={})",
            plan.channels,
            plan.missing.join("|"),
            plan.dropped.join("|")
        ));
    }
    Ok(detail.join(" "))
}

fn check_status_contract() -> Result<String, String> {
    use crate::status::{render, FlowStatus, Snapshot};
    let flows = [
        FlowStatus {
            index: 0,
            offset: 0,
            channels: 8,
            dest: "239.69.67.1:5067".to_string(),
            ssrc: 0x4e53_5034,
            payload_type: 97,
        },
        FlowStatus {
            index: 1,
            offset: 8,
            channels: 2,
            dest: "239.69.67.1:5069".to_string(),
            ssrc: 0x4e53_5035,
            payload_type: 97,
        },
    ];
    let base = Snapshot {
        mode: "normal",
        group: "239.69.67.1",
        port: 5067,
        maxch: 10,
        layout: "7.1.2",
        codec: "pcm",
        stream_id: 7,
        fmt_name: "PCM_16",
        rate: 48_000,
        channels: 10,
        packets: 123,
        send_err: 0,
        bytes_sent: 1024,
        silent: 0,
        late: 0,
        dropped: 0,
        ssrc: 0x4e53_5034,
        payload_type: 97,
        dest: "239.69.67.1:5067".to_string(),
        encoding: "L24",
        flow_count: 2,
        split_mode: "standard",
        flows: &flows,
        rs_in: 0,
        rs_out: 0,
        src_active: true,
        src_age_ms: 0,
        src_codec: "pcm",
        src_rate: 48_000,
        levels: &[],
        ptime_us: 1_000,
        split: true,
        ring_fill: 0,
        fifo_bytes: 0,
        locked: false,
        timestamping: 0,
        offset_ns: 0.0,
        rate_ppm: 0.0,
        fit_rms_ns: 0.0,
        ptp_mode: "local",
        sap: false,
        sap_interval_s: 30,
        bits: None,
    };
    let standard = render(&base);
    for wanted in [
        "\"layout\":\"7.1.2\"",
        "\"maxch\":10",
        "\"flows\":2",
        "\"split_mode\":\"standard\"",
        "\"split\":1",
        "\"flow_details\":[",
        "\"dest\":\"239.69.67.1:5067\"",
        "\"ssrc\":\"0x4e535034\"",
    ] {
        assert_that(standard.contains(wanted), || {
            format!("status.json missing {wanted:?}: {standard}")
        })?;
    }
    assert_that(standard.matches("\"index\":").count() == 2, || {
        format!("flow_details count: {standard}")
    })?;
    let legacy = Snapshot {
        split_mode: "legacy",
        split: false,
        ..base
    };
    let legacy_text = render(&legacy);
    assert_that(legacy_text.contains("\"split_mode\":\"legacy\""), || {
        format!("legacy split_mode: {legacy_text}")
    })?;
    assert_that(legacy_text.contains("\"split\":0"), || {
        format!("legacy split flag: {legacy_text}")
    })?;
    Ok("layout/maxch/flows=2/split_mode=standard|legacy/split compat/flow_details[]".to_string())
}

fn check_sdp_required() -> Result<String, String> {
    let text = describe_for(5000, 2, RtpEncoding::L24, 1);
    for required in [
        "v=0\r\n",
        "o=- ",
        "s=AES67Bridge compliance flow0\r\n",
        "c=IN IP4 127.0.0.1",
        "t=0 0\r\n",
        "m=audio 5000 RTP/AVP 97\r\n",
        "a=rtpmap:97 L24/48000/2\r\n",
        "a=sendonly\r\n",
        "a=ptime:",
    ] {
        assert_that(text.contains(required), || {
            format!("missing SDP line {required:?}")
        })?;
    }
    Ok("v/o/s/c/t/m + rtpmap + ptime + sendonly present".to_string())
}

fn check_sdp_ptime_format() -> Result<String, String> {
    let mut detail = Vec::new();
    for (ptime, expected) in [
        (125u32, "a=ptime:0.125\r\n"),
        (250, "a=ptime:0.25\r\n"),
        (500, "a=ptime:0.5\r\n"),
        (1_000, "a=ptime:1\r\n"),
    ] {
        let output = nsp_aes67::rtp::Output::open(
            sdp_output(5000, 2, RtpEncoding::L24, false),
            RtpEncoding::L24,
        )
        .expect("output");
        let labels = vec!["CH1".to_string(), "CH2".to_string()];
        let text = sdp::describe(
            "t",
            &output,
            0,
            &labels,
            None,
            1,
            1,
            PacketTiming::new(crate::OUT_RATE, ptime).expect("timing"),
            None,
        );
        assert_that(text.contains(expected), || {
            format!("ptime {ptime}: missing {expected:?} in {text}")
        })?;
        detail.push(format!("{ptime}us->{}", expected.trim()));
    }
    Ok(detail.join(" "))
}

fn check_sdp_local_refclk() -> Result<String, String> {
    let output = nsp_aes67::rtp::Output::open(
        sdp_output(5000, 2, RtpEncoding::L24, false),
        RtpEncoding::L24,
    )
    .expect("output");
    let labels = vec!["CH1".to_string(), "CH2".to_string()];
    let text = sdp::describe_local(
        "t",
        &output,
        0,
        &labels,
        1,
        1,
        PacketTiming::new(crate::OUT_RATE, 1_000).expect("timing"),
        None,
    );
    assert_that(text.contains("a=ts-refclk:local\r\n"), || {
        "local ts-refclk missing".to_string()
    })?;
    assert_that(text.contains("a=mediaclk:direct=0\r\n"), || {
        "mediaclk missing".to_string()
    })?;
    assert_that(!text.contains("IEEE1588-2008"), || {
        "local mode must not claim PTP".to_string()
    })?;
    Ok("ts-refclk:local + mediaclk:direct=0, no PTP claim".to_string())
}

fn check_sdp_ptp_refclk() -> Result<String, String> {
    let output = nsp_aes67::rtp::Output::open(
        sdp_output(5000, 2, RtpEncoding::L24, false),
        RtpEncoding::L24,
    )
    .expect("output");
    let labels = vec!["CH1".to_string(), "CH2".to_string()];
    let text = sdp::describe(
        "t",
        &output,
        0,
        &labels,
        Some(sdp::Reference {
            gm: [0xab; 8],
            domain: 42,
        }),
        1,
        1,
        PacketTiming::new(crate::OUT_RATE, 1_000).expect("timing"),
        None,
    );
    assert_that(
        text.contains("a=ts-refclk:ptp=IEEE1588-2008:AB-AB-AB-AB-AB-AB-AB-AB:42\r\n"),
        || "PTP gmid/domain missing".to_string(),
    )?;
    assert_that(text.contains("a=mediaclk:direct=0\r\n"), || {
        "mediaclk missing".to_string()
    })?;
    assert_that(text.contains("a=clock-domain:PTPv2 42\r\n"), || {
        "clock-domain missing".to_string()
    })?;
    Ok("ts-refclk:ptp=gmid:domain + mediaclk:direct=0".to_string())
}

fn check_sdp_multiflow_version() -> Result<String, String> {
    let first = describe_for(5000, 8, RtpEncoding::L24, 1);
    let second = describe_for(5002, 2, RtpEncoding::L24, 2);
    assert_that(
        first.matches("m=audio ").count() == 1 && second.matches("m=audio ").count() == 1,
        || "each flow SDP must carry one m= section".to_string(),
    )?;
    assert_that(
        first.contains("m=audio 5000") && second.contains("m=audio 5002"),
        || "per-flow port".to_string(),
    )?;
    let version_a = describe_for(5000, 2, RtpEncoding::L24, 1);
    let version_b = describe_for(5000, 2, RtpEncoding::L24, 2);
    assert_that(version_a != version_b, || {
        "o= version must change the session".to_string()
    })?;
    Ok("one m= per flow, per-flow port, o= version bump".to_string())
}

fn check_sap_config() -> Result<String, String> {
    let mut cfg = base_config(5000, 2);
    cfg.sap = true;
    cfg.sap_interval_s = 30;
    let timing = timing(&cfg);
    let plan = plan_flows(&cfg, &timing, RtpEncoding::L24, 2, 1)?;
    let session = build_session_config(
        &cfg,
        Ipv4Addr::LOCALHOST,
        &timing,
        RtpEncoding::L24,
        2,
        &plan,
    )?;
    assert_that(session.sap, || "sap flag".to_string())?;
    assert_that(
        session.sap_destination == Ipv4Addr::new(224, 2, 127, 254),
        || format!("SAP destination {}", session.sap_destination),
    )?;
    assert_that(
        session.sap_interval == Duration::from_secs(30),
        || "SAP interval".to_string(),
    )?;
    Ok("SAP enabled: 224.2.127.254:9875 every 30s".to_string())
}

fn check_dscp() -> Result<String, String> {
    assert_that(nsp_aes67::net::TOS_AF41 == 0x88, || {
        format!("AF41 TOS 0x{:02x}", nsp_aes67::net::TOS_AF41)
    })?;
    assert_that(nsp_aes67::net::TOS_EF == 0xb8, || {
        format!("EF TOS 0x{:02x}", nsp_aes67::net::TOS_EF)
    })?;
    // Best effort socket assertion: some Windows stacks do not expose IP_TOS
    // through getsockopt even though setsockopt succeeds.
    let flow = sdp_output(5000, 2, RtpEncoding::L24, false);
    let output = nsp_aes67::rtp::Output::open(flow, RtpEncoding::L24)
        .map_err(|err| format!("output: {err}"))?;
    let tos = match output.socket_tos() {
        Ok(tos) => {
            assert_that(tos == 0x88, || format!("socket TOS 0x{tos:02x}"))?;
            format!("socket TOS 0x{tos:02x}")
        }
        Err(err) => format!("socket query unsupported ({err}); constants verified"),
    };
    Ok(format!("AF41=0x88 EF=0xb8 {tos}"))
}

fn check_pacing_grid() -> Result<String, String> {
    let timing = PacketTiming::new(crate::OUT_RATE, 1_000).expect("timing");
    let mut grid = Grid::new(timing);
    let ms = 1_000_000i128;
    // First call arms the next slot.
    assert_that(grid.due(100 * ms, 8).is_none(), || "arming".to_string())?;
    let mut stamps = Vec::new();
    for step in 1..=4 {
        let batch = grid
            .due((100 + step) * ms, 8)
            .ok_or_else(|| format!("slot {} not due", 100 + step))?;
        assert_that(batch.slots == 1, || format!("burst slots {}", batch.slots))?;
        for slot in batch.slot_range() {
            stamps.push(timing.timestamp(slot));
        }
    }
    let deltas: Vec<u32> = stamps.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert_that(deltas.iter().all(|&delta| delta == 48), || {
        format!("non uniform timestamp deltas {deltas:?}")
    })?;
    // A late wake may catch up at most max_catch_up+1 slots (no unbounded burst).
    let late = grid
        .due(120 * ms, 3)
        .ok_or_else(|| "late batch".to_string())?;
    assert_that(late.slots <= 4, || format!("unbounded burst {}", late.slots))?;
    Ok(format!(
        "uniform 48-sample steps, catch-up batch {} slots",
        late.slots
    ))
}

fn check_loopback_l24() -> Result<String, String> {
    let port = ports(1);
    let mut cfg = base_config(port, 2);
    cfg.ptime_us = 1_000;
    // The frame count is derived from 120 whole packets, not 120 frames.
    let frames_pkt = timing(&cfg).samples_per_packet() as usize;
    let frames = sine_frames(frames_pkt * 120, 2, 48_000, 440.0);
    let (packets, expected, _plan, _) = loopback_case(cfg, 2, &frames, Duration::from_millis(300))?;
    let start = trim_leading_zero_packets(&packets);
    let content = &packets[start..];
    assert_that(content.len() >= 100, || {
        format!(
            "only {} content packets ({} recorded, first payload sizes {:?})",
            content.len(),
            packets.len(),
            packets.iter().take(3).map(|p| p.payload.len()).collect::<Vec<_>>()
        )
    })?;
    check_seq_ts(&packets, frames_pkt)?;
    for (index, packet) in content.iter().take(100).enumerate() {
        assert_that(packet.pt == 97, || format!("PT {}", packet.pt))?;
        assert_that(!packet.marker, || "unexpected marker".to_string())?;
        assert_that(packet.payload.len() == frames_pkt * 2 * 3, || {
            format!("payload {}", packet.payload.len())
        })?;
        let begin = index * frames_pkt * 2 * 3;
        assert_that(
            packet.payload == expected[begin..begin + frames_pkt * 2 * 3],
            || format!("payload mismatch packet {index}"),
        )?;
    }
    Ok(format!(
        "120 packets L24/48k @{frames_pkt}f: seq+{frames_pkt}ts, {}B payload, PCM bit-exact ({} content packets)",
        frames_pkt * 2 * 3,
        content.len()
    ))
}

fn check_loopback_l16() -> Result<String, String> {
    let port = ports(1);
    let mut cfg = base_config(port, 2);
    cfg.encoding = Encoding::L16;
    cfg.ptime_us = 1_000;
    let frames_pkt = timing(&cfg).samples_per_packet() as usize;
    let frames = sine_frames(frames_pkt * 120, 2, 48_000, 440.0);
    let (packets, expected, _plan, _) = loopback_case(cfg, 2, &frames, Duration::from_millis(300))?;
    let start = trim_leading_zero_packets(&packets);
    let content = &packets[start..];
    assert_that(content.len() >= 100, || "content packets".to_string())?;
    assert_that(content[0].pt == 96, || format!("PT {}", content[0].pt))?;
    for (index, packet) in content.iter().take(80).enumerate() {
        assert_that(packet.payload.len() == frames_pkt * 2 * 2, || {
            format!("payload {}", packet.payload.len())
        })?;
        let begin = index * frames_pkt * 2 * 2;
        assert_that(
            packet.payload == expected[begin..begin + frames_pkt * 2 * 2],
            || format!("L16 payload mismatch packet {index}"),
        )?;
    }
    Ok(format!(
        "120 packets L16/PT96 @{frames_pkt}f: {}B MSB-first payload, PCM bit-exact",
        frames_pkt * 2 * 2
    ))
}

fn check_loopback_ptimes() -> Result<String, String> {
    let mut detail = Vec::new();
    for ptime in [125u32, 250, 500] {
        let port = ports(1);
        let mut cfg = base_config(port, 2);
        cfg.ptime_us = ptime;
        let frames_pkt = (48_000u64 * u64::from(ptime) / 1_000_000) as usize;
        let count = if ptime == 125 { 200 } else { 120 };
        let frames = sine_frames(frames_pkt * count, 2, 48_000, 440.0);
        let total_frames = frames.len();
        let settle = Duration::from_millis((count as u64 * u64::from(ptime)) / 1000 + 250);
        let (packets, expected, _plan, _) = loopback_case(cfg, 2, &frames, settle)?;
        let start = trim_leading_zero_packets(&packets);
        let content = &packets[start..];
        assert_that(!content.is_empty(), || format!("{ptime}us: no packets"))?;
        let bytes = frames_pkt * 2 * 3;
        let first_ts = content[0].ts;
        let mut mapped = 0usize;
        let mut seq_gaps = 0usize;
        let mut previous_seq: Option<u16> = None;
        let mut last_ts = first_ts;
        // Packets are addressed by RTP timestamp, not by recording order: the
        // scheduler drops whole pacing slots when the host cannot hold a 125us
        // grid (Windows timer granularity is about 1ms), so each payload is
        // placed by its media-clock position instead of assuming a gapless
        // recording.
        for packet in content {
            assert_that(packet.payload.len() == bytes, || {
                format!("{ptime}us payload {}", packet.payload.len())
            })?;
            let delta = packet.ts.wrapping_sub(first_ts);
            assert_that(delta % frames_pkt as u32 == 0, || {
                format!("{ptime}us ts step {delta}")
            })?;
            assert_that(!packet.marker, || format!("{ptime}us marker"))?;
            // UDP loopback can lose a datagram on a loaded host, so require
            // monotonic (never duplicate/reordered) sequence numbers and record
            // the count instead of failing the compliance row for host loss.
            if let Some(previous) = previous_seq {
                let step = packet.seq.wrapping_sub(previous);
                assert_that(step != 0 && step < 0x8000, || {
                    format!("{ptime}us seq order step={step}")
                })?;
                if step > 1 {
                    seq_gaps += usize::from(step - 1);
                }
            }
            previous_seq = Some(packet.seq);
            last_ts = packet.ts;
            let offset = delta as usize;
            if offset + frames_pkt <= total_frames {
                let begin = offset * 2 * 3;
                assert_that(packet.payload == expected[begin..begin + bytes], || {
                    format!("{ptime}us PCM mismatch at frame {offset}")
                })?;
                mapped += 1;
            }
        }
        assert_that(mapped >= 40.min(content.len()), || {
            format!("{ptime}us only {mapped} timestamp-mapped packets")
        })?;
        assert_that(
            last_ts.wrapping_sub(first_ts) as usize >= frames_pkt * 20,
            || format!("{ptime}us media clock did not advance"),
        )?;
        detail.push(format!(
            "{ptime}us:{frames_pkt}f/pkt/{mapped}pkt/{}gaps",
            seq_gaps
        ));
    }
    Ok(detail.join(" "))
}

fn check_loopback_multiflow() -> Result<String, String> {
    let port = ports(2);
    let mut cfg = base_config(port, 10);
    cfg.ptime_us = 1_000;
    let timing = timing(&cfg);
    let frames_pkt = timing.samples_per_packet() as usize;
    let plan = plan_flows(&cfg, &timing, RtpEncoding::L24, 10, 1)?;
    assert_that(plan.len() == 2, || "10ch must use 2 flows".to_string())?;
    let frames = sine_frames(frames_pkt * 120, 10, 48_000, 330.0);
    let engine = Engine::start(cfg, 10)?;
    let mut recorders = Vec::new();
    for flow in &engine.plan {
        recorders.push(start_recorder(flow.dest.port())?);
    }
    let _ = plan;
    engine.submit_packets(&frames, frames_pkt)?;
    thread::sleep(Duration::from_millis(300));
    let channels = engine.channels;
    let flows = engine.plan.clone();
    engine.stop();
    thread::sleep(Duration::from_millis(80));
    let mut recordings = Vec::new();
    for recorder in recorders {
        recordings.push(recorder.finish());
    }
    assert_that(recordings.iter().all(|list| !list.is_empty()), || {
        "flow without packets".to_string()
    })?;
    for (index, packets) in recordings.iter().enumerate() {
        let flow = &flows[index];
        let expected =
            expected_payload(&frames, channels, flow.offset, flow.channels, RtpEncoding::L24);
        let mut got = Vec::new();
        let start = trim_leading_zero_packets(packets);
        for packet in packets[start..].iter().take(80) {
            got.extend_from_slice(&packet.payload);
        }
        assert_that(
            got.len() >= expected.len().min(80 * flow.channels * frames_pkt * 3),
            || "short multi-flow recording".to_string(),
        )?;
        let compare = got.len().min(expected.len());
        assert_that(got[..compare] == expected[..compare], || {
            format!("flow {index} channel data mismatch")
        })?;
        assert_that(
            packets
                .iter()
                .all(|packet| packet.payload.len() == frames_pkt * flow.channels * 3),
            || format!("flow {index} payload size"),
        )?;
    }
    Ok("10ch -> 2 flows (8ch + 2ch), per-flow PCM slices bit-exact".to_string())
}

fn check_loopback_legacy_split() -> Result<String, String> {
    let port = ports(1);
    let mut cfg = base_config(port, 16);
    cfg.legacy_split = true;
    cfg.mtu = 1500;
    cfg.ptime_us = 1_000;
    let frames_pkt = timing(&cfg).samples_per_packet() as usize;
    let frames = sine_frames(frames_pkt * 120, 16, 48_000, 275.0);
    let engine = Engine::start(cfg, 16)?;
    let plan = engine.plan.clone();
    assert_that(plan.len() == 1, || "legacy mode is one flow".to_string())?;
    let recorder = start_recorder(plan[0].dest.port())?;
    engine.submit_packets(&frames, frames_pkt)?;
    thread::sleep(Duration::from_millis(300));
    engine.stop();
    thread::sleep(Duration::from_millis(80));
    let packets = recorder.finish();
    let start = trim_leading_zero_packets(&packets);
    let content = &packets[start..];
    assert_that(content.len() >= 100, || {
        format!("only {} split fragments", content.len())
    })?;
    let expected = expected_payload(&frames, 16, 0, 16, RtpEncoding::L24);
    let mut reassembled = Vec::new();
    let mut intervals = 0usize;
    let mut index = 0usize;
    while index + 1 < content.len().min(202) {
        let first = &content[index];
        let second = &content[index + 1];
        assert_that(first.ts == second.ts, || "split fragments share ts".to_string())?;
        assert_that(second.seq == first.seq.wrapping_add(1), || {
            "split seq continuity".to_string()
        })?;
        assert_that(!first.marker, || "non-final fragment must have M=0".to_string())?;
        assert_that(second.marker, || "final fragment must have M=1".to_string())?;
        assert_that(first.payload.len() <= 1440 && second.payload.len() <= 1440, || {
            "fragment payload budget".to_string()
        })?;
        reassembled.extend_from_slice(&first.payload);
        reassembled.extend_from_slice(&second.payload);
        intervals += 1;
        index += 2;
    }
    assert_that(intervals >= 50, || format!("only {intervals} intervals"))?;
    assert_that(reassembled == expected[..reassembled.len()], || {
        "split reassembly != submitted PCM".to_string()
    })?;
    Ok(format!(
        "{intervals} intervals x 2 fragments, same ts/seq+1/M0->M1, 1440B budget, reassembly bit-exact"
    ))
}

fn check_loopback_sap() -> Result<String, String> {
    let port = ports(1);
    let mut cfg = base_config(port, 2);
    cfg.sap = true;
    cfg.sap_interval_s = 30;
    // Keep the SAP traffic on the loopback so the test can observe it.
    cfg.sap_destination = Some(Ipv4Addr::LOCALHOST);
    let sap_receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 9875))
        .map_err(|err| format!("bind SAP 9875: {err}"))?;
    sap_receiver
        .set_read_timeout(Some(Duration::from_millis(300)))
        .map_err(|err| format!("sap timeout: {err}"))?;
    let engine = Engine::start(cfg, 2)?;
    let mut buffer = [0u8; 2048];
    let mut announcements = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && announcements.len() < 2 {
        match sap_receiver.recv(&mut buffer) {
            Ok(size) => announcements.push(buffer[..size].to_vec()),
            Err(_) => {}
        }
    }
    assert_that(!announcements.is_empty(), || "no SAP announcement".to_string())?;
    let first = &announcements[0];
    assert_that(first[0] == 0x20, || format!("SAP header 0x{:02x}", first[0]))?;
    assert_that(first[1] == 0x00, || "SAP auth len 0".to_string())?;
    let text = String::from_utf8_lossy(first);
    assert_that(text.contains("application/sdp"), || "SAP payload type".to_string())?;
    assert_that(text.contains("v=0") && text.contains("m=audio"), || {
        "SAP body SDP".to_string()
    })?;
    // Stopping withdraws the announcement (RFC 2974 delete, three repeats).
    engine.stop();
    let mut deletions = 0usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && deletions < 1 {
        match sap_receiver.recv(&mut buffer) {
            Ok(size) => {
                if buffer[0] == 0x24 {
                    deletions += 1;
                    let text = String::from_utf8_lossy(&buffer[..size]);
                    assert_that(text.contains("o=- "), || "SAP delete origin".to_string())?;
                }
            }
            Err(_) => {}
        }
    }
    assert_that(deletions >= 1, || "no SAP deletion on stop".to_string())?;
    Ok(format!(
        "SAP v2 (127.0.0.1:9875) announce (0x20, application/sdp, SDP body) + delete (0x24) on stop; {} announcements",
        announcements.len()
    ))
}

fn check_loopback_44k1() -> Result<String, String> {
    // The SRC itself is unit-tested; here assert the end-to-end 44.1->48 frame
    // count and that no full-scale discontinuity is produced.
    use crate::resample::{PolyphaseResampler, Resampler, RubatoResampler};
    for kind in ["rubato", "c-poly"] {
        let mut resampler: Box<dyn Resampler> = if kind == "rubato" {
            Box::new(RubatoResampler::new(2).unwrap())
        } else {
            Box::new(PolyphaseResampler::new())
        };
        let frames = sine_frames(44_100, 2, 44_100, 997.0);
        let mut peak = 0i32;
        let mut out_frames = 0u64;
        {
            let mut sink = |frame: &[i32; MAX_CH]| {
                peak = peak.max(frame[0].abs()).max(frame[1].abs());
                out_frames += 1;
            };
            for frame in &frames {
                resampler.push(frame, 2, &mut sink);
            }
            resampler.flush(2, &mut sink);
        }
        assert_that(resampler.rs_in() == 44_100, || format!("{kind} rs_in"))?;
        assert_that(out_frames == 48_000, || {
            format!("{kind} output frames {out_frames}")
        })?;
        assert_that(peak < 4_200_000, || {
            format!("{kind} full-scale discontinuity peak {peak}")
        })?;
    }
    Ok("rubato and c-poly: 44100 -> 48000 frames, bounded output".to_string())
}

// ---------------------------------------------------------------------------
// runner
// ---------------------------------------------------------------------------

pub fn run_all() -> Vec<Check> {
    let mut checks = Vec::new();
    checks.push(run("AES67-01-ptime-frames", check_ptime_frames));
    checks.push(run("AES67-02-encoding-payload-type", check_encoding_pt));
    checks.push(run("AES67-03-standard-flow-packing", check_standard_flow_packing));
    checks.push(run("AES67-04-flow-identity", check_flow_identity));
    checks.push(run("AES67-05-no-split-refuse", check_no_split_reject));
    checks.push(run("AES67-06-channel-map-labels", check_channel_map_labels));
    checks.push(run("AES67-07-sdp-required-lines", check_sdp_required));
    checks.push(run("AES67-08-sdp-ptime-format", check_sdp_ptime_format));
    checks.push(run("AES67-09-sdp-local-refclk", check_sdp_local_refclk));
    checks.push(run("AES67-10-sdp-ptp-refclk", check_sdp_ptp_refclk));
    checks.push(run("AES67-11-sdp-multiflow-version", check_sdp_multiflow_version));
    checks.push(run("AES67-12-sap-config", check_sap_config));
    checks.push(run("AES67-13-dscp", check_dscp));
    checks.push(run("AES67-14-pacing-grid", check_pacing_grid));
    checks.push(run("AES67-15-loopback-l24-1ms", check_loopback_l24));
    checks.push(run("AES67-16-loopback-l16-1ms", check_loopback_l16));
    checks.push(run("AES67-17-loopback-ptimes", check_loopback_ptimes));
    checks.push(run("AES67-18-loopback-multiflow", check_loopback_multiflow));
    checks.push(run("AES67-19-loopback-legacy-split", check_loopback_legacy_split));
    checks.push(run("AES67-20-sap-announce-delete", check_loopback_sap));
    checks.push(run("AES67-21-src-44k1-to-48k", check_loopback_44k1));
    checks.push(run("AES67-22-layouts-packing-labels", check_layout_matrix));
    checks.push(run("AES67-23-status-contract", check_status_contract));
    checks.push(run("AES67-24-layout-intersection", check_layout_intersection));
    checks
}
