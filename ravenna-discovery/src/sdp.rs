//! SDP session descriptions for AES67 / RAVENNA media streams.
//!
//! Facts encoded here (sources in artifacts/aes67/ravenna-mdns-rtsp-2026-11-24.md):
//!
//! * RFC 4566 baseline (v/o/s/c/t/m, a=rtpmap, a=ptime, a=sendonly).
//! * RAVENNA Operating Principles (Draft 1.0) section 3.3.3: session-level
//!   a=clock-domain:PTPv2 <domain> ("RAVENNA streams SHALL always include this
//!   attribute") and stream-level a=sync-time:<u32> (the RTP timestamp that
//!   corresponds to the PTP epoch).
//! * RFC 7273 clock signalling used by AES67: a=ts-refclk: and a=mediaclk:.
//! * a=framecount:<samples per packet>: used by AES67/RAVENNA implementations
//!   (Merging/bondagit aes67-linux-daemon emits it for every source).
//!
//! The deliberately conservative default is a plain AES67 description
//! (ts-refclk:local, no clock-domain, no sync-time): announcing PTP while the
//! RTP timestamps are still sample counters would be a lie that makes receivers
//! drop the stream. RAVENNA mode must be enabled explicitly and only when the
//! sender really stamps RTP with PTP-epoch frames.

use std::fmt::Write as _;
use std::net::IpAddr;

/// Payload encoding. RAVENNA requires L16 and L24 for audio nodes (section 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    L16,
    L24,
}

impl Encoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L16 => "L16",
            Self::L24 => "L24",
        }
    }

    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::L16 => 2,
            Self::L24 => 3,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "L16" => Some(Self::L16),
            "L24" => Some(Self::L24),
            _ => None,
        }
    }
}

/// Clock reference for a=ts-refclk (RFC 7273).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsRefClk {
    /// Free-running local clock: honest placeholder when no PTP servo exists.
    Local,
    /// ts-refclk:ptp=IEEE1588-2008:<gmid>:<domain>.
    Ptp { gmid: [u8; 8], domain: u8 },
}

impl TsRefClk {
    pub fn render(self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::Ptp { gmid, domain } => {
                let hex: Vec<String> = gmid.iter().map(|b| format!("{b:02x}")).collect();
                format!("ptp=IEEE1588-2008:{}:{domain}", hex.join("-"))
            }
        }
    }
}

/// One m=audio stream.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub dest: IpAddr,
    pub port: u16,
    pub ttl: u8,
    pub payload_type: u8,
    pub encoding: Encoding,
    pub sample_rate: u32,
    pub channels: u16,
    /// Packet time in milliseconds (a=ptime): 1.0 = 1 ms, 0.125 = 125 us.
    pub ptime_ms: f64,
    pub ssrc: u32,
    pub sendonly: bool,
    pub media_clock_direct: bool,
    /// RAVENNA a=sync-time; only rendered when the session declares a clock domain.
    pub sync_time: Option<u32>,
    /// Emit a=framecount with the samples-per-packet value (AES67 daemon style).
    pub frame_count: bool,
}

impl StreamConfig {
    /// Samples of one channel carried per RTP packet.
    pub fn samples_per_packet(&self) -> u32 {
        ((self.sample_rate as f64) * self.ptime_ms / 1000.0).round() as u32
    }

    /// RTP payload size in bytes (may exceed 1440 B for large channel counts).
    pub fn packet_bytes(&self) -> usize {
        self.samples_per_packet() as usize * self.channels as usize * self.encoding.bytes_per_sample()
    }
}

/// One SDP session (one rtsp://.../by-name/<session_name> endpoint).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub session_name: String,
    pub description: Option<String>,
    pub origin_addr: IpAddr,
    pub session_id: u64,
    pub session_version: u64,
    /// Some(domain) -> emit RAVENNA a=clock-domain:PTPv2 <domain>.
    /// Leave None unless RTP timestamps are PTP-epoch based.
    pub clock_domain: Option<u8>,
    pub ts_refclk: TsRefClk,
    pub streams: Vec<StreamConfig>,
}

fn render_number(v: f64) -> String {
    if (v.fract()).abs() < 1e-9 {
        format!("{}", v as u64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

impl SessionConfig {
    /// Render the SDP document served by RTSP DESCRIBE.
    pub fn to_sdp(&self) -> String {
        let mut out = String::new();
        out.push_str("v=0\r\n");
        let _ = write!(
            out,
            "o=- {} {} IN IP4 {}\r\n",
            self.session_id, self.session_version, self.origin_addr
        );
        let _ = write!(out, "s={}\r\n", self.session_name);
        if let Some(d) = &self.description {
            let _ = write!(out, "i={d}\r\n");
        }
        if let Some(first) = self.streams.first() {
            let _ = write!(out, "c=IN IP4 {}/{}\r\n", first.dest, first.ttl);
        }
        out.push_str("t=0 0\r\n");
        if let Some(domain) = self.clock_domain {
            let _ = write!(out, "a=clock-domain:PTPv2 {domain}\r\n");
        }
        for s in &self.streams {
            let _ = write!(out, "m=audio {} RTP/AVP {}\r\n", s.port, s.payload_type);
            let _ = write!(
                out,
                "a=rtpmap:{} {}/{}/{}\r\n",
                s.payload_type,
                s.encoding.as_str(),
                s.sample_rate,
                s.channels
            );
            let _ = write!(out, "a=ptime:{}\r\n", render_number(s.ptime_ms));
            out.push_str(if s.sendonly { "a=sendonly\r\n" } else { "a=recvonly\r\n" });
            let _ = write!(
                out,
                "a=ssrc:{} cname:{}@{}\r\n",
                s.ssrc, self.session_name, self.origin_addr
            );
            if s.media_clock_direct {
                out.push_str("a=mediaclk:direct=0\r\n");
            }
            let _ = write!(out, "a=ts-refclk:{}\r\n", self.ts_refclk.render());
            if s.frame_count {
                let _ = write!(out, "a=framecount:{}\r\n", s.samples_per_packet());
            }
            if let Some(n) = s.sync_time {
                // Only meaningful together with a declared synchronization source.
                if self.clock_domain.is_some() {
                    let _ = write!(out, "a=sync-time:{n}\r\n");
                }
            }
        }
        out
    }
}

/// Everything the RTSP DESCRIBE handler needs: fn sdp(&self) -> String.
///
/// Integration point for the NSP nsp_aes67 crate: implement this trait by calling
/// nsp_aes67_sdp(handle, flow_index, ...) (see INTERFACE.md), or by returning the
/// output of the session's with_sdp. The rest of this prototype does not care
/// where the document comes from.
pub trait SdpProvider: Send + Sync {
    fn sdp(&self) -> String;
}

/// Serve a fixed SDP document (from a file, or from the C daemon's *.sdp).
#[derive(Debug, Clone)]
pub struct StaticSdp(pub String);

impl SdpProvider for StaticSdp {
    fn sdp(&self) -> String {
        self.0.clone()
    }
}

/// Serve a generated SDP document.
#[derive(Debug, Clone)]
pub struct SessionSdp(pub SessionConfig);

impl SdpProvider for SessionSdp {
    fn sdp(&self) -> String {
        self.0.to_sdp()
    }
}
