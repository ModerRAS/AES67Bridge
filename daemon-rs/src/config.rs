//! daemon.conf + CLI parsing. CLI overrides the config file, C v5.3 semantics.
//!
//! New in the Rust daemon (all default to the C behaviour): `encoding` (L24/L16),
//! `sap`, `source_filter`, `labels`, `channel_map`, `local_ip`, `resampler`,
//! `ptp` and `pt`.

use std::fmt;
use std::net::Ipv4Addr;

use crate::platform;

pub const MAX_CH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    L24,
    L16,
}

impl Encoding {
    pub fn bytes(self) -> usize {
        match self {
            Encoding::L24 => 3,
            Encoding::L16 => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Encoding::L24 => "L24",
            Encoding::L16 => "L16",
        }
    }

    pub fn default_pt(self) -> u8 {
        match self {
            // AES67 media profile defaults: L16 = 96 ... 95?, L24 = 97 (RFC 8331/3190
            // registration used by AES67-2018 table 2 and by the bridge C daemon).
            Encoding::L16 => 96,
            Encoding::L24 => 97,
        }
    }

    pub fn from_str(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "l24" | "24" | "pcm24" | "24bit" => Some(Encoding::L24),
            "l16" | "16" | "pcm16" | "16bit" => Some(Encoding::L16),
            _ => None,
        }
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Speaker layouts offered by the AES67Bridge TV UI. The layout is the single
/// source of truth for the channel count and for the default channel labels;
/// the conf maxch key is redundant and must agree with it.
pub fn layout_channels(layout: &str) -> Option<usize> {
    match layout.trim().to_ascii_lowercase().as_str() {
        "2.0" => Some(2),
        "4.1.2" => Some(7),
        "5.1" => Some(6),
        "5.1.2" => Some(8),
        "7.1" => Some(8),
        "7.1.2" => Some(10),
        "5.1.4" => Some(10),
        "7.1.4" => Some(12),
        "9.1.6" => Some(16),
        _ => None,
    }
}

/// Default AES67 channel labels per layout, in UI meter order.
pub fn layout_labels(layout: &str) -> Option<&'static [&'static str]> {
    Some(match layout.trim().to_ascii_lowercase().as_str() {
        "2.0" => &["L", "R"][..],
        "4.1.2" => &["L", "R", "LFE", "Ls", "Rs", "Tfl", "Tfr"][..],
        "5.1" => &["L", "R", "C", "LFE", "Ls", "Rs"][..],
        "5.1.2" => &["L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr"][..],
        "7.1" => &["L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb"][..],
        "7.1.2" => &[
            "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr",
        ][..],
        "5.1.4" => &[
            "L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr", "Tbl", "Tbr",
        ][..],
        "7.1.4" => &[
            "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr",
        ][..],
        "9.1.6" => &[
            "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Lw", "Rw", "Tfl", "Tfr", "Tbl",
            "Tbr", "Tsl", "Tsr",
        ][..],
        _ => return None,
    })
}

/// Layout whose label set equals \`labels\` ignoring order (used by the
/// compressed-bitstream decoder, whose coded channel order differs from the
/// AES67 canonical order for the same speaker set).
pub fn layout_for_label_set(labels: &[String]) -> Option<&'static str> {
    let mut sorted: Vec<&str> = labels.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    for name in LAYOUT_NAMES {
        if let Some(expected) = layout_labels(name) {
            let mut want: Vec<&str> = expected.to_vec();
            want.sort_unstable();
            if want == sorted {
                return Some(name);
            }
        }
    }
    None
}

/// Best-effort layout name for a raw channel count (status.json when the conf
/// has no layout key). Ambiguous counts follow the UI default preference.
pub fn derive_layout(channels: usize) -> &'static str {
    match channels {
        2 => "2.0",
        6 => "5.1",
        7 => "4.1.2",
        8 => "7.1",
        10 => "7.1.2",
        12 => "7.1.4",
        16 => "9.1.6",
        _ => "",
    }
}

/// All layout names offered by the TV UI, in UI order.
pub const LAYOUT_NAMES: [&str; 9] = [
    "2.0", "4.1.2", "5.1", "5.1.2", "7.1", "7.1.2", "5.1.4", "7.1.4", "9.1.6",
];

/// Canonical layout name whose label list equals `labels` exactly, if any.
pub fn layout_for_labels(labels: &[&str]) -> Option<&'static str> {
    for name in LAYOUT_NAMES {
        if let Some(expected) = layout_labels(name) {
            if expected.len() == labels.len()
                && expected.iter().zip(labels.iter()).all(|(a, b)| a == b)
            {
                return Some(name);
            }
        }
    }
    None
}

/// Actual wire layout after intersecting the configured target layout (the
/// maximum the backend declared) with the source layout (what the input block
/// really carries).
///
/// Contract (lead decision 2026-11-24):
/// - the output is the intersection of target and source channel labels, in
///   source order;
/// - target channels that the source does not have are NOT filled and never
///   reported (no fake channels);
/// - source channels outside the target are dropped by default (no downmix)
///   and the caller logs a warning;
/// - SDP/status must report `output_layout` (for example target 5.1.2 with a
///   5.1 source is reported as 5.1, never as 5.1.2).
#[derive(Clone, Debug)]
pub struct LayoutPlan {
    pub target: String,
    pub source: String,
    pub output_layout: String,
    pub source_channels: usize,
    pub channels: usize,
    pub labels: Vec<String>,
    /// Output channel -> source channel index (vendor ring map).
    pub map: Vec<usize>,
    /// Target channel names the source does not carry (not filled).
    pub missing: Vec<String>,
    /// Source channel names outside the target (dropped, no downmix).
    pub dropped: Vec<String>,
}

/// Resolve the actual output layout for one stream shape.
///
/// `source_layout` is the optional `layout=` key from live.meta; when absent the
/// source layout is inferred from the channel count (see `derive_layout`).
pub fn plan_layout(
    target: &str,
    source_channels: usize,
    source_layout: Option<&str>,
) -> Result<LayoutPlan, String> {
    let source_channels = source_channels.clamp(1, MAX_CH);
    let target = target.trim().to_string();
    if target.is_empty() {
        // Legacy C contract: no target layout, output everything the source has.
        let inferred = derive_layout(source_channels);
        return Ok(LayoutPlan {
            target,
            source: inferred.to_string(),
            output_layout: inferred.to_string(),
            source_channels,
            channels: source_channels,
            labels: (0..source_channels).map(|c| format!("CH{}", c + 1)).collect(),
            map: (0..source_channels).collect(),
            missing: Vec::new(),
            dropped: Vec::new(),
        });
    }
    let target_labels = layout_labels(&target)
        .ok_or_else(|| format!("unknown layout '{target}'"))?
        .to_vec();
    let declared_source = source_layout
        .map(|layout| layout.trim().to_string())
        .filter(|layout| !layout.is_empty());
    let resolved_source = declared_source
        .as_deref()
        .and_then(|layout| layout_labels(layout).map(|labels| (layout, labels)))
        .filter(|(layout, _)| layout_channels(layout) == Some(source_channels));
    let Some((source_name, source_labels)) = resolved_source else {
        // Source layout unknown: fall back to the first N target channels and
        // report a "custom" layout, so no channel is invented.
        let keep = source_channels.min(target_labels.len());
        let labels: Vec<String> = target_labels[..keep].iter().map(|s| s.to_string()).collect();
        let output_layout = layout_for_labels(&labels.iter().map(String::as_str).collect::<Vec<_>>())
            .unwrap_or("custom")
            .to_string();
        let missing: Vec<String> = target_labels[keep..].iter().map(|s| s.to_string()).collect();
        let mut dropped = Vec::new();
        if source_channels > target_labels.len() {
            for extra in target_labels.len()..source_channels {
                dropped.push(format!("CH{}", extra + 1));
            }
        }
        return Ok(LayoutPlan {
            target,
            source: String::new(),
            output_layout,
            source_channels,
            channels: keep,
            labels,
            map: (0..keep).collect(),
            missing,
            dropped,
        });
    };
    let mut labels = Vec::new();
    let mut map = Vec::new();
    let mut dropped = Vec::new();
    for (index, label) in source_labels.iter().enumerate() {
        if target_labels.iter().any(|target| target == label) {
            labels.push((*label).to_string());
            map.push(index);
        } else {
            dropped.push((*label).to_string());
        }
    }
    if labels.is_empty() {
        return Err(format!(
            "target layout {target} and source layout {source_name} share no channels"
        ));
    }
    let missing: Vec<String> = target_labels
        .iter()
        .filter(|target| !source_labels.iter().any(|source| source == *target))
        .map(|s| s.to_string())
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let output_layout = layout_for_labels(&label_refs).unwrap_or("custom").to_string();
    Ok(LayoutPlan {
        target,
        source: source_name.to_string(),
        output_layout,
        source_channels,
        channels: labels.len(),
        labels,
        map,
        missing,
        dropped,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResamplerKind {
    /// rubato sinc resampler (optional; the live 44.1k path has an open
    /// white-noise report since 2026-11-26, the daemon logs a WARN when selected).
    Rubato,
    /// C-daemon-compatible 160 phase x 287 tap Q30 polyphase FIR, bit-exact
    /// against `aes67_rtp_sender_v4.c` for A/B verification. Default since 0.2.4.
    CPoly,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub fifo: String,
    pub meta: String,
    pub sdp: String,
    /// HTTP port that serves the current SDP at /aes67.sdp (0 disables it).
    /// Controllers that do not use SAP can pull the description instead.
    pub http_port: u16,
    pub status: String,
    pub conf: String,
    pub lock: String,
    pub group: String,
    pub port: u16,
    pub ptime_us: u32,
    /// Redundant channel cap; a configured layout overrides it.
    pub maxch: usize,
    /// Raw compressed elementary stream exported by the HAL tee (live.bits).
    pub bits: String,
    /// Companion metadata for live.bits (stream_id/fmt/codec_guess/chmask).
    pub bits_meta: String,
    /// Optional diagnostics: tee the consumed live.bits bytes / submitted PCM.
    pub dump_bits: String,
    pub dump_pcm: String,
    /// Speaker layout from daemon.conf (2.0/4.1.2/5.1/5.1.2/7.1/7.1.2/5.1.4/
    /// 7.1.4/9.1.6); empty keeps the legacy maxch/meta behaviour.
    pub layout: String,
    /// Layout adapter mode. Only `strict` is implemented today (target and
    /// source channel labels intersect, no up/downmix); any other value is
    /// refused instead of silently enabling a missing adapter.
    pub layout_mode: String,
    /// Render PCM through ROAR (channel-based element with the real source layout,
    /// IAMF downmix for legal target pairs, EAR otherwise). Default true: the wire
    /// layout becomes the configured target layout and the bridge owns downmix.
    pub pcm_render: bool,
    /// Apply TrueHD dialogue normalisation as a constant pre-render gain. Default true;
    /// code 31 (reference, -31 dBFS) is 0 dB, code 27 is -4 dB.
    pub dialnorm: bool,
    /// User output trim in dB, applied before the render limiter on every path
    /// (PCM render, E-AC-3 decode and TrueHD objects). Negative values create
    /// headroom; positive values are capped by the limiter. Default 0.0.
    pub gain_db: f32,
    /// ROAR look-ahead limiter ceiling in dBFS (default -1.0, ROAR default).
    /// Applies to the PCM render and TrueHD object paths.
    pub limiter_db: f32,
    pub codec: String,
    pub encoding: Encoding,
    pub mtu: usize,
    pub no_split: bool,
    /// C v5.3 single-flow + RTP fragmentation mode (A/B reference mode).
    /// Default false: AES67 standard multi-flow packing (<= 8ch per flow).
    pub legacy_split: bool,
    pub buffer_ms: usize,
    /// Release an idle RTP session after this many ms of source silence.
    /// 0 = never release once the session is up (silence keeps flowing instead of
    /// paying a session restart / PTP re-lock gap on every source hiccup).
    pub auto_idle_ms: u64,
    pub prebuffer_ms: usize,
    pub local_ip: Option<Ipv4Addr>,
    pub resampler: ResamplerKind,
    pub sap: bool,
    pub sap_interval_s: u32,
    /// SAP announcement group; `None` uses 224.2.127.254 (AES67 default).
    pub sap_destination: Option<Ipv4Addr>,
    pub source_filter: bool,
    pub labels: Vec<String>,
    pub channel_map: Vec<usize>,
    pub ptp: bool,
    pub ptp_domain: u8,
    pub ptp_master: Option<Ipv4Addr>,
    pub ptp_interface: Option<Ipv4Addr>,
    pub force_ch: usize,
    pub force_rate: u32,
    pub seconds: f64,
    pub sched_fifo: i32,
    pub trace_data: usize,
    pub pt: u8,
    pub ssrc: Option<u32>,
    pub log: Option<String>,
    /// Non-fatal parsing notes (for example maxch overridden by layout) logged
    /// once by the daemon at startup.
    pub notes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fifo: crate::DEFAULT_FIFO.to_string(),
            meta: crate::DEFAULT_META.to_string(),
            sdp: crate::DEFAULT_SDP.to_string(),
            http_port: 0,
            status: crate::DEFAULT_STATUS.to_string(),
            conf: crate::DEFAULT_CONF.to_string(),
            lock: crate::DEFAULT_LOCK.to_string(),
            group: crate::DEFAULT_GROUP.to_string(),
            port: crate::DEFAULT_PORT,
            ptime_us: 1000,
            maxch: 8,
            bits: "/data/haltee/live.bits".to_string(),
            bits_meta: "/data/haltee/live.bits.meta".to_string(),
            dump_bits: String::new(),
            dump_pcm: String::new(),
            layout: String::new(),
            layout_mode: "strict".to_string(),
            pcm_render: true,
            dialnorm: true,
            gain_db: 0.0,
            limiter_db: -1.0,
            codec: "pcm".to_string(),
            encoding: Encoding::L24,
            mtu: crate::DEFAULT_MTU,
            no_split: false,
            legacy_split: false,
            buffer_ms: 100,
        auto_idle_ms: 0,
            prebuffer_ms: 20,
            local_ip: None,
            resampler: ResamplerKind::CPoly,
            sap: false,
            sap_interval_s: 30,
            sap_destination: None,
            source_filter: false,
            labels: Vec::new(),
            channel_map: Vec::new(),
            ptp: false,
            ptp_domain: 0,
            ptp_master: None,
            ptp_interface: None,
            force_ch: 0,
            force_rate: 0,
            seconds: 0.0,
            sched_fifo: 0,
            trace_data: 0,
            pt: 0, // resolved from encoding after parsing
            ssrc: None,
            log: None,
            notes: Vec::new(),
        }
    }
}

impl Config {
    pub fn frames_pkt(&self) -> usize {
        ((u64::from(crate::OUT_RATE) * u64::from(self.ptime_us)) / 1_000_000) as usize
    }

    pub fn period_ns(&self) -> i64 {
        i64::from(self.ptime_us) * 1_000
    }

    pub fn payload_type(&self) -> u8 {
        if self.pt == 0 {
            self.encoding.default_pt()
        } else {
            self.pt
        }
    }

    /// Channel count implied by the configured layout (None when unset).
    pub fn layout_channel_count(&self) -> Option<usize> {
        layout_channels(&self.layout)
    }

    /// Layout name for status.json and the UI level meters.
    pub fn effective_layout(&self) -> String {
        let configured = self.layout.trim();
        if configured.is_empty() {
            derive_layout(self.maxch).to_string()
        } else {
            configured.to_string()
        }
    }

    /// Apply the layout-is-the-only-truth contract: a valid layout overrides
    /// maxch and records a note instead of silently disagreeing with it.
    pub fn resolve_layout(&mut self) {
        self.layout = self.layout.trim().to_string();
        if self.layout.is_empty() {
            return;
        }
        if let Some(channels) = layout_channels(&self.layout) {
            if self.maxch != channels {
                self.notes.push(format!(
                    "layout {} implies {} channels but maxch={}; using layout, maxch is redundant",
                    self.layout, channels, self.maxch
                ));
                self.maxch = channels;
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.maxch < 2 || self.maxch > MAX_CH {
            return Err(format!("--maxch {} out of range 2..{}", self.maxch, MAX_CH));
        }
        if !matches!(
            self.codec.trim().to_ascii_lowercase().as_str(),
            "pcm" | "bits" | "l16" | "l24" | "pcm16" | "pcm24" | "auto" | "eac3" | "truehd"
        ) {
            return Err(format!(
                "--codec {} (pcm|bits|l16|l24|auto|eac3|truehd)",
                self.codec
            ));
        }
        if !self.layout_mode.eq_ignore_ascii_case("strict") {
            return Err(format!(
                "layout_mode '{}' rejected: only 'strict' (target/source label intersection) is implemented; up/downmix adapters are not available",
                self.layout_mode
            ));
        }
        if !self.layout.is_empty() && layout_channels(&self.layout).is_none() {
            return Err(format!(
                "unknown layout '{}' (2.0|4.1.2|5.1|5.1.2|7.1|7.1.2|5.1.4|7.1.4|9.1.6)",
                self.layout
            ));
        }
        if self.ptp_domain > 127 {
            return Err(format!("--ptp-domain {} out of range 0..127", self.ptp_domain));
        }
        if self.port == 0 {
            return Err(format!("--port {}", self.port));
        }
        if !matches!(self.ptime_us, 125 | 250 | 500 | 1000) {
            return Err(format!(
                "unsupported --ptime-us {} (allowed 125/250/500/1000)",
                self.ptime_us
            ));
        }
        if self.mtu < 200 {
            return Err(format!("invalid --mtu {}", self.mtu));
        }
        if self.buffer_ms < 8 || self.buffer_ms > 1024 {
            return Err(format!("--buffer-ms {} out of range 8..1024", self.buffer_ms));
        }
        if self.prebuffer_ms > self.buffer_ms {
            return Err(format!(
                "--prebuffer-ms {} exceeds --buffer-ms {}",
                self.prebuffer_ms, self.buffer_ms
            ));
        }
        if self.frames_pkt() < 1 || self.frames_pkt() > 48 {
            return Err("ptime too large".to_string());
        }
        if platform::parse_ipv4(&self.group).is_none() {
            return Err(format!("invalid --group {}", self.group));
        }
        for (id, label) in self.labels.iter().enumerate() {
            if label.is_empty() || label.len() > 32 || label.contains(',') || label.contains('\n') {
                return Err(format!("invalid label #{id} '{}'", label));
            }
        }
        Ok(())
    }

    pub fn group_ip(&self) -> Ipv4Addr {
        platform::parse_ipv4(&self.group).unwrap_or(Ipv4Addr::new(239, 69, 67, 1))
    }

    /// Allowed ptime list for --help.
    pub fn usage(program: &str) -> String {
        format!(
            "{program} usage: --fifo F --meta M [--seconds N] [--sched-fifo PRIO] [--sdp PATH] \
[--status PATH] [--config PATH] [--lock PATH] [--group G] [--port P] \
[--ptime-us 125|250|500|1000] [--maxch 2..16] [--layout 7.1] \
[--layout-mode strict] [--pcm-render 1|0] [--dialnorm 1|0] [--gain-db X] [--limiter-db X] \
[--bits-meta /data/haltee/live.bits.meta] \
[--codec pcm|bits|auto|eac3|truehd|l16] [--encoding l24|l16] [--pt N] [--mtu 1500] [--target-layout L] \
[--split] [--no-split] [--legacy-split] [--buffer-ms 100] [--prebuffer-ms 20] [--auto-idle-ms 0] \
[--ch N] [--rate R] \
[--local-ip IP|auto] [--resampler rubato|c-poly] [--sap] [--sap-interval S] [--http-port P] \
[--source-filter] [--labels L,R,...] [--channel-map 0,1,...] \
[--ptp] [--ptp-domain D] [--ptp-master IP|--ptp-gm IP] [--ptp-interface IP] \
[--ssrc HEX] [--log PATH] [--trace-data N]"
        )
    }
}

fn trim_quotes(text: &str) -> &str {
    text.trim().trim_matches('"').trim_matches('\'')
}

fn apply_key_value(cfg: &mut Config, key: &str, value: &str, origin: &str) -> Result<(), String> {
    match key {
        "group" => cfg.group = value.to_string(),
        "port" => {
            cfg.port = value
                .parse()
                .map_err(|_| format!("{origin}: bad port '{value}'"))?
        }
        "ptime_us" | "ptime" => {
            cfg.ptime_us = value
                .parse()
                .map_err(|_| format!("{origin}: bad ptime_us '{value}'"))?
        }
        "maxch" => {
            cfg.maxch = value
                .parse()
                .map_err(|_| format!("{origin}: bad maxch '{value}'"))?
        }
        "http_port" | "sdp_port" => {
            cfg.http_port = value
                .trim()
                .parse()
                .map_err(|_| format!("{origin}: bad http_port '{value}'"))?
        }
        "layout" => cfg.layout = value.trim().to_string(),
        "bits" => cfg.bits = value.to_string(),
        "bits_meta" => cfg.bits_meta = value.to_string(),
        "layout_mode" => cfg.layout_mode = value.trim().to_string(),
        "pcm_render" => cfg.pcm_render = parse_bool(value),
        "dialnorm" => cfg.dialnorm = parse_bool(value),
        "gain_db" => {
            cfg.gain_db = value
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("{origin}: bad gain_db '{value}'"))?
                .clamp(-60.0, 12.0);
        }
        "limiter_db" => {
            cfg.limiter_db = value
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("{origin}: bad limiter_db '{value}'"))?
                .clamp(-24.0, 0.0);
        }
        "split" => {
            // UI contract (lead decision 2026-11-24): split=1 is AES67 standard
            // multi-flow packing (flows of up to 8 channels, >8 channel sources
            // use a second flow); split=0 is the single-flow extension with C
            // v5.3 RTP fragmentation (same timestamp, seq+1, marker on the last
            // fragment). The CLI keeps --no-split for the separate
            // one-packet-per-interval refusal mode.
            if parse_bool(value) {
                cfg.legacy_split = false;
                cfg.no_split = false;
            } else {
                cfg.legacy_split = true;
                cfg.no_split = false;
            }
        }
        "codec" => {
            cfg.codec = value.to_string();
            match value.trim().to_ascii_lowercase().as_str() {
                "l16" | "pcm16" | "16" => cfg.encoding = Encoding::L16,
                "l24" | "pcm24" | "24" => cfg.encoding = Encoding::L24,
                _ => {}
            }
        }
        "encoding" => {
            cfg.encoding = Encoding::from_str(value)
                .ok_or_else(|| format!("{origin}: bad encoding '{value}' (l24|l16)"))?
        }
        "mtu" => {
            cfg.mtu = value
                .parse()
                .map_err(|_| format!("{origin}: bad mtu '{value}'"))?
        }
        "legacy_split" => cfg.legacy_split = parse_bool(value),
        "auto_idle_ms" => {
            cfg.auto_idle_ms = value
                .parse()
                .map_err(|_| format!("{origin}: bad auto_idle_ms '{value}'"))?
        }
        "buffer_ms" => {
            cfg.buffer_ms = value
                .parse()
                .map_err(|_| format!("{origin}: bad buffer_ms '{value}'"))?
        }
        "prebuffer_ms" => {
            cfg.prebuffer_ms = value
                .parse()
                .map_err(|_| format!("{origin}: bad prebuffer_ms '{value}'"))?
        }
        "sap" => cfg.sap = parse_bool(value),
        "sap_destination" => {
            cfg.sap_destination = Some(
                platform::parse_ipv4(value)
                    .ok_or_else(|| format!("{origin}: bad sap_destination '{value}'"))?,
            )
        }
        "sap_interval_s" => {
            cfg.sap_interval_s = value
                .parse()
                .map_err(|_| format!("{origin}: bad sap_interval_s '{value}'"))?
        }
        "source_filter" => cfg.source_filter = parse_bool(value),
        "labels" => {
            cfg.labels = value
                .split(',')
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect()
        }
        "channel_map" => {
            cfg.channel_map = value
                .split(',')
                .map(|v| v.trim().parse::<usize>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| format!("{origin}: bad channel_map '{value}'"))?
        }
        "local_ip" => {
            cfg.local_ip = if value == "auto" || value.is_empty() {
                None
            } else {
                Some(
                    platform::parse_ipv4(value)
                        .ok_or_else(|| format!("{origin}: bad local_ip '{value}'"))?,
                )
            }
        }
        "resampler" => {
            cfg.resampler = match value {
                "rubato" => ResamplerKind::Rubato,
                "c-poly" | "cpoly" | "poly" => ResamplerKind::CPoly,
                _ => return Err(format!("{origin}: bad resampler '{value}' (rubato|c-poly)")),
            }
        }
        "ptp" => cfg.ptp = parse_bool(value),
        "ptp_domain" => {
            cfg.ptp_domain = value
                .parse()
                .map_err(|_| format!("{origin}: bad ptp_domain '{value}'"))?
        }
        "ptp_master" | "ptp_gm" => {
            // ptp_gm is the TV UI key; an empty value means "no pinned GM".
            cfg.ptp_master = if value.trim().is_empty() {
                None
            } else {
                Some(
                    platform::parse_ipv4(value)
                        .ok_or_else(|| format!("{origin}: bad ptp_gm '{value}'"))?,
                )
            }
        }
        "ptp_interface" => {
            cfg.ptp_interface = Some(
                platform::parse_ipv4(value)
                    .ok_or_else(|| format!("{origin}: bad ptp_interface '{value}'"))?,
            )
        }
        "pt" => {
            cfg.pt = value
                .parse()
                .map_err(|_| format!("{origin}: bad pt '{value}'"))?
        }
        "ssrc" => {
            let hex = value.trim_start_matches("0x").trim_start_matches("0X");
            cfg.ssrc = Some(
                u32::from_str_radix(hex, 16)
                    .map_err(|_| format!("{origin}: bad ssrc '{value}' (hex)"))?,
            )
        }
        _ => { /* unknown keys are ignored, like the C daemon */ }
    }
    Ok(())
}

fn parse_bool(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn load_config_file(cfg: &mut Config, path: &str) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        apply_key_value(cfg, key.trim(), trim_quotes(value), path)?;
    }
    Ok(())
}

/// Parse CLI. Two passes like the C daemon: `--config` first, then every flag,
/// so CLI values win over the config file. Returns None when --help was handled.
pub fn parse_args(args: &[String]) -> Result<Option<Config>, String> {
    let mut cfg = Config::default();
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--config" && index + 1 < args.len() {
            cfg.conf = args[index + 1].clone();
        }
        index += 1;
    }
    if !cfg.conf.is_empty() {
        let conf_path = cfg.conf.clone();
        load_config_file(&mut cfg, &conf_path)?;
    }
    let program = args.first().map(String::as_str).unwrap_or("aes67-daemon");
    let mut pt_forced = false;
    let mut index = 1;
    let next = |index: &mut usize, args: &[String]| -> Result<String, String> {
        *index += 1;
        args.get(*index)
            .cloned()
            .ok_or_else(|| format!("missing value for '{}'", args[*index - 1]))
    };
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" | "-h" => {
                println!("{}", Config::usage(program));
                return Ok(None);
            }
            "--fifo" => cfg.fifo = next(&mut index, args)?,
            "--meta" => cfg.meta = next(&mut index, args)?,
            "--seconds" => {
                cfg.seconds = next(&mut index, args)?.parse().unwrap_or(0.0);
            }
            "--sched-fifo" => {
                cfg.sched_fifo = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--trace-data" => {
                cfg.trace_data = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--sdp" => cfg.sdp = next(&mut index, args)?,
            "--status" => cfg.status = next(&mut index, args)?,
            "--config" => {
                cfg.conf = next(&mut index, args)?;
            }
            "--lock" => cfg.lock = next(&mut index, args)?,
            "--group" => cfg.group = next(&mut index, args)?,
            "--port" => {
                cfg.port = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--ptime-us" | "--ptime" => {
                cfg.ptime_us = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--maxch" => {
                cfg.maxch = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--layout" | "--target-layout" => cfg.layout = next(&mut index, args)?,
            "--bits" => cfg.bits = next(&mut index, args)?,
            "--bits-meta" => cfg.bits_meta = next(&mut index, args)?,
            "--dump-bits" => cfg.dump_bits = next(&mut index, args)?,
            "--dump-pcm" => cfg.dump_pcm = next(&mut index, args)?,
            "--layout-mode" => cfg.layout_mode = next(&mut index, args)?,
            "--pcm-render" => {
                cfg.pcm_render = parse_bool(&next(&mut index, args)?);
            }
            "--dialnorm" => {
                cfg.dialnorm = parse_bool(&next(&mut index, args)?);
            }
            "--gain-db" => {
                cfg.gain_db = next(&mut index, args)?.parse().unwrap_or(0.0);
            }
            "--limiter-db" => {
                cfg.limiter_db = next(&mut index, args)?.parse().unwrap_or(-1.0);
            }
            "--split" => {
                cfg.legacy_split = false;
                cfg.no_split = false;
            }
            "--codec" => {
                let value = next(&mut index, args)?;
                match value.trim().to_ascii_lowercase().as_str() {
                    "l16" | "pcm16" | "16" => cfg.encoding = Encoding::L16,
                    "l24" | "pcm24" | "24" => cfg.encoding = Encoding::L24,
                    _ => {}
                }
                cfg.codec = value;
            }
            "--encoding" => {
                cfg.encoding = Encoding::from_str(&next(&mut index, args)?)
                    .ok_or_else(|| "--encoding l24|l16".to_string())?;
            }
            "--pt" => {
                cfg.pt = next(&mut index, args)?.parse().unwrap_or(0);
                pt_forced = true;
            }
            "--mtu" => {
                cfg.mtu = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--buffer-ms" => {
                cfg.auto_idle_ms = next(&mut index, args)?.parse().unwrap_or(0);
        cfg.buffer_ms = next(&mut index, args)?.parse().unwrap_or(100);
            }
            "--prebuffer-ms" => {
                cfg.prebuffer_ms = next(&mut index, args)?.parse().unwrap_or(10);
            }
            "--no-split" => cfg.no_split = true,
            "--legacy-split" => cfg.legacy_split = true,
            "--ch" => {
                cfg.force_ch = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--rate" => {
                cfg.force_rate = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--local-ip" => {
                let value = next(&mut index, args)?;
                cfg.local_ip = if value == "auto" || value.is_empty() {
                    None
                } else {
                    Some(platform::parse_ipv4(&value).ok_or("--local-ip needs an IPv4 address")?)
                };
            }
            "--resampler" => {
                cfg.resampler = match next(&mut index, args)?.as_str() {
                    "rubato" => ResamplerKind::Rubato,
                    "c-poly" | "cpoly" | "poly" => ResamplerKind::CPoly,
                    other => return Err(format!("--resampler {other} (rubato|c-poly)")),
                }
            }
            "--http-port" => {
                cfg.http_port = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--sap" => cfg.sap = true,
            "--sap-interval" => {
                cfg.sap_interval_s = next(&mut index, args)?.parse().unwrap_or(30);
            }
            "--sap-dest" => {
                cfg.sap_destination = Some(
                    platform::parse_ipv4(&next(&mut index, args)?)
                        .ok_or("--sap-dest needs an IPv4 address")?,
                );
            }
            "--source-filter" => cfg.source_filter = true,
            "--labels" => {
                cfg.labels = next(&mut index, args)?
                    .split(',')
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect();
            }
            "--channel-map" => {
                cfg.channel_map = next(&mut index, args)?
                    .split(',')
                    .map(|v| v.trim().parse::<usize>())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "--channel-map 0,1,...".to_string())?;
            }
            "--ptp" => cfg.ptp = true,
            "--ptp-domain" => {
                cfg.ptp_domain = next(&mut index, args)?.parse().unwrap_or(0);
            }
            "--ptp-master" | "--ptp-gm" => {
                cfg.ptp_master = Some(
                    platform::parse_ipv4(&next(&mut index, args)?)
                        .ok_or("--ptp-master needs an IPv4 address")?,
                );
            }
            "--ptp-interface" => {
                cfg.ptp_interface = Some(
                    platform::parse_ipv4(&next(&mut index, args)?)
                        .ok_or("--ptp-interface needs an IPv4 address")?,
                );
            }
            "--ssrc" => {
                let value = next(&mut index, args)?;
                let hex = value.trim_start_matches("0x").trim_start_matches("0X");
                cfg.ssrc = Some(
                    u32::from_str_radix(hex, 16).map_err(|_| "--ssrc HEX".to_string())?,
                );
            }
            "--log" => cfg.log = Some(next(&mut index, args)?),
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
        index += 1;
    }
    let _ = pt_forced;
    cfg.resolve_layout();
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT_TABLE: &[(&str, usize, &[&str])] = &[
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
                "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Lw", "Rw", "Tfl", "Tfr",
                "Tbl", "Tbr", "Tsl", "Tsr",
            ],
        ),
    ];

    #[test]
    fn layout_table_matches_the_tv_ui_contract() {
        for &(layout, channels, labels) in LAYOUT_TABLE {
            assert_eq!(layout_channels(layout), Some(channels), "{layout} channels");
            let got: Vec<&str> = layout_labels(layout).unwrap().to_vec();
            assert_eq!(got, labels, "{layout} labels");
            // 8ch/10ch are ambiguous layouts (7.1 vs 5.1.2, 7.1.2 vs 5.1.4);
            // the status fallback only has to round-trip to a valid layout.
            let derived = derive_layout(channels);
            assert_eq!(layout_channels(derived), Some(channels), "{layout} derived");
        }
    }

    #[test]
    fn layout_overrides_redundant_maxch_and_records_a_note() {
        let mut cfg = Config::default();
        cfg.layout = " 7.1.2 ".to_string();
        cfg.maxch = 8;
        cfg.resolve_layout();
        assert_eq!(cfg.layout, "7.1.2");
        assert_eq!(cfg.maxch, 10);
        assert_eq!(cfg.effective_layout(), "7.1.2");
        assert_eq!(cfg.notes.len(), 1, "{:?}", cfg.notes);
        // Second resolve must not warn again.
        cfg.notes.clear();
        cfg.resolve_layout();
        assert!(cfg.notes.is_empty());
        // Unknown layouts are rejected by validate, not silently mapped.
        cfg.layout = "6.1".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn conf_file_keys_drive_layout_split_codec_and_ptp() {
        // R:\Temp (the session TEMP) is not writable for child processes, so
        // place the fixture next to the test executable.
        let path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("aes67-config-test-{}.conf", std::process::id()));
        std::fs::write(
            &path,
            "layout=5.1.2\nsplit=0\ncodec=l16\nptp=1\nptp_gm=192.168.1.10\nptp_domain=24\nmaxch=6\n",
        )
        .unwrap();
        let mut cfg = Config::default();
        load_config_file(&mut cfg, path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        cfg.resolve_layout();
        assert_eq!(cfg.layout, "5.1.2");
        assert_eq!(cfg.maxch, 8, "layout wins over maxch=6");
        assert!(!cfg.notes.is_empty());
        assert_eq!(cfg.encoding, Encoding::L16, "codec=l16");
        assert!(cfg.legacy_split && !cfg.no_split, "split=0 is legacy RTP fragmentation");
        assert_eq!(cfg.ptp_master, Some(Ipv4Addr::new(192, 168, 1, 10)));
        assert_eq!(cfg.ptp_domain, 24);
        assert!(cfg.ptp);
        cfg.validate().unwrap();
    }

    #[test]
    fn split_conf_selects_standard_or_legacy_wire_mode() {
        let mut cfg = Config::default();
        apply_key_value(&mut cfg, "split", "1", "test").unwrap();
        assert!(!cfg.no_split && !cfg.legacy_split, "split=1 is standard multi-flow");
        apply_key_value(&mut cfg, "split", "0", "test").unwrap();
        assert!(cfg.legacy_split && !cfg.no_split, "split=0 is legacy RTP fragmentation");
    }

    fn strings(values: &[String]) -> Vec<&str> {
        values.iter().map(String::as_str).collect()
    }

    #[test]
    fn layout_intersection_reports_the_actual_output() {
        // target 5.1.2 + source 5.1 -> actual 5.1 (height channels not invented)
        let p = plan_layout("5.1.2", 6, Some("5.1")).unwrap();
        assert_eq!(p.output_layout, "5.1");
        assert_eq!(p.channels, 6);
        assert_eq!(strings(&p.labels), ["L", "R", "C", "LFE", "Ls", "Rs"]);
        assert_eq!(p.map, [0, 1, 2, 3, 4, 5]);
        assert_eq!(strings(&p.missing), ["Tfl", "Tfr"]);
        assert!(p.dropped.is_empty());
        // target 7.1.2 + source 7.1 -> actual 7.1 (back pair kept, heights absent)
        let p = plan_layout("7.1.2", 8, Some("7.1")).unwrap();
        assert_eq!(p.output_layout, "7.1");
        assert_eq!(p.channels, 8);
        assert_eq!(
            strings(&p.labels),
            ["L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb"]
        );
        assert_eq!(strings(&p.missing), ["Tfl", "Tfr"]);
        // target 7.1.2 + source 5.1 -> actual 5.1
        let p = plan_layout("7.1.2", 6, Some("5.1")).unwrap();
        assert_eq!(p.output_layout, "5.1");
        assert_eq!(p.channels, 6);
        assert_eq!(strings(&p.missing), ["Lb", "Rb", "Tfl", "Tfr"]);
        // target 5.1 + source 5.1.2 -> drop heights, no downmix
        let p = plan_layout("5.1", 8, Some("5.1.2")).unwrap();
        assert_eq!(p.output_layout, "5.1");
        assert_eq!(p.channels, 6);
        assert_eq!(p.map, [0, 1, 2, 3, 4, 5]);
        assert_eq!(strings(&p.dropped), ["Tfl", "Tfr"]);
        // target 7.1 + source 5.1.2 -> intersection is plain 5.1
        let p = plan_layout("7.1", 8, Some("5.1.2")).unwrap();
        assert_eq!(p.output_layout, "5.1");
        assert_eq!(p.channels, 6);
        assert_eq!(strings(&p.dropped), ["Tfl", "Tfr"]);
        // Identity when source equals the target layout.
        for &(layout, channels, _) in LAYOUT_TABLE {
            let p = plan_layout(layout, channels, Some(layout)).unwrap();
            assert_eq!(p.output_layout, layout, "{layout}");
            assert_eq!(p.channels, channels, "{layout}");
            assert!(p.missing.is_empty() && p.dropped.is_empty(), "{layout}");
        }
        // Source layout unknown: no channel is invented, layout becomes custom
        // unless the prefix itself is a named layout.
        let p = plan_layout("5.1", 4, None).unwrap();
        assert_eq!(p.channels, 4);
        assert_eq!(p.output_layout, "custom");
        assert_eq!(strings(&p.missing), ["Ls", "Rs"]);
    }

    #[test]
    fn cli_layout_codec_and_ptp_gm_aliases_parse() {
        let args = vec![
            "aes67-daemon".to_string(),
            "--layout".into(),
            "7.1.2".into(),
            "--codec".into(),
            "l16".into(),
            "--ptp-gm".into(),
            "192.168.63.215".into(),
            "--ptp-domain".into(),
            "0".into(),
            "--group".into(),
            "239.100.5.7".into(),
            "--port".into(),
            "6500".into(),
        ];
        let cfg = parse_args(&args).unwrap().unwrap();
        assert_eq!(cfg.layout, "7.1.2");
        assert_eq!(cfg.maxch, 10);
        assert_eq!(cfg.encoding, Encoding::L16);
        assert_eq!(cfg.ptp_master, Some(Ipv4Addr::new(192, 168, 63, 215)));
        assert_eq!(cfg.group, "239.100.5.7");
        assert_eq!(cfg.port, 6500);
        cfg.validate().unwrap();
    }
}