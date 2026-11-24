//! Node-level configuration and small platform helpers.

use std::net::{IpAddr, UdpSocket};

use anyhow::{bail, Context, Result};

use crate::sdp::{Encoding, SessionConfig, StreamConfig, TsRefClk};

pub const DEFAULT_RTSP_PORT: u16 = 8554;
pub const DEFAULT_HTTP_PORT: u16 = 8080;

/// A RAVENNA node: one RTSP/HTTP service plus the sessions it advertises.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub node_id: String,
    pub host_name: String,
    pub local_ip: IpAddr,
    pub rtsp_port: u16,
    pub http_port: u16,
    pub sessions: Vec<SessionConfig>,
}

impl NodeConfig {
    pub fn rtsp_url(&self, session_name: &str) -> String {
        format!(
            "rtsp://{}:{}/by-name/{}",
            self.local_ip, self.rtsp_port, session_name
        )
    }
}

/// Best-effort primary LAN address.
///
/// No packet is sent: connect() on a UDP socket only performs a route lookup,
/// which is how straightforward "which interface would I use" helpers do it.
pub fn detect_local_ip() -> Result<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").context("bind ephemeral UDP socket")?;
    sock.connect("192.0.2.1:9")
        .context("select primary IPv4 route (TEST-NET-1)")?;
    Ok(sock.local_addr()?.ip())
}

/// <hostname>.local. as required for mDNS host records.
pub fn default_host_name() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "aes67bridge".to_string());
    let sane: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("{}.local.", sane.to_ascii_lowercase())
}

/// Parse a PTP grandmaster clock identity.
///
/// Accepts the IEEE 1588 text form (ae6f0d.fffe.8e86fc) and MAC-ish forms
/// (ae:6f:0d:ff:fe:8e:86:fc, ae-6f-...). This is the identity observed on the
/// lab network's grandmaster (192.168.63.215, domain 0, two-step, E2E).
pub fn parse_gmid(s: &str) -> Result<[u8; 8]> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if cleaned.len() != 16 {
        bail!("PTP grandmaster id must contain 16 hex digits (got {cleaned:?} from {s:?})");
    }
    let mut out = [0u8; 8];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16)
            .with_context(|| format!("invalid hex byte in grandmaster id {s:?}"))?;
    }
    Ok(out)
}

/// Builder for the single-stream SDP used by the prototype CLI.
#[allow(clippy::too_many_arguments)]
pub fn build_session(
    session_name: &str,
    session_id: u64,
    origin_addr: IpAddr,
    dest: IpAddr,
    rtp_port: u16,
    payload_type: u8,
    encoding: Encoding,
    sample_rate: u32,
    channels: u16,
    ptime_ms: f64,
    ssrc: u32,
    clock_domain: Option<u8>,
    ts_refclk: TsRefClk,
) -> SessionConfig {
    let stream = StreamConfig {
        dest,
        port: rtp_port,
        ttl: 1,
        payload_type,
        encoding,
        sample_rate,
        channels,
        ptime_ms,
        ssrc,
        sendonly: true,
        media_clock_direct: true,
        sync_time: clock_domain.map(|_| 0),
        frame_count: true,
    };
    SessionConfig {
        session_name: session_name.to_string(),
        description: Some("AES67Bridge RAVENNA discovery prototype".to_string()),
        origin_addr,
        session_id,
        session_version: 0,
        clock_domain,
        ts_refclk,
        streams: vec![stream],
    }
}
