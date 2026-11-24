//! Minimal RTSP 1.0 server: the RAVENNA connection-management side of the prototype.
//!
//! Method matrix (see artifacts/aes67/ravenna-mdns-rtsp-2026-11-24.md):
//!   OPTIONS       -> Public list (RFC 2326 10.1)
//!   DESCRIBE      -> 200 + application/sdp; unknown session -> 404 but the
//!                    connection is kept open (RAVENNA Operating Principles 3.4.1)
//!   SETUP         -> Transport echo (unicast client_port / multicast destination)
//!   PLAY          -> 200 + Session + RTP-Info, invokes the PlaybackHook
//!   TEARDOWN      -> 200, stops the hook
//!   GET_PARAMETER -> 200 (RTSP keep-alive)
//!   SET_PARAMETER -> 200
//!   ANNOUNCE      -> 200 (client ack); server-initiated ANNOUNCE is pushed to
//!                    every connection that has done DESCRIBE when new SDP is available.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::rtsp_proto::{parse_request, RtspRequest, RtspResponse, RTSP_VERSION};
use crate::sdp::SdpProvider;

/// One RTSP-addressable session (a named stream).
#[derive(Clone)]
pub struct RtspSession {
    pub id: u8,
    pub name: String,
    pub sdp: Arc<dyn SdpProvider>,
}

/// Transport negotiated by SETUP.
#[derive(Debug, Clone, Default)]
pub struct SetupTransport {
    pub multicast: bool,
    /// Client RTP port (unicast) or stream port (multicast).
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub destination: Option<IpAddr>,
    pub ttl: Option<u8>,
}

impl SetupTransport {
    pub fn render(&self, server_rtp_port: u16, ssrc: u32) -> String {
        if self.multicast {
            let dest = self
                .destination
                .map(|d| d.to_string())
                .unwrap_or_else(|| "0.0.0.0".to_string());
            let ttl = self.ttl.unwrap_or(1);
            format!(
                "RTP/AVP;multicast;destination={dest};port={};ttl={ttl};ssrc={ssrc:08X}",
                self.rtp_port
            )
        } else {
            format!(
                "RTP/AVP;unicast;client_port={}-{};server_port={}-{};ssrc={ssrc:08X}",
                self.rtp_port,
                self.rtcp_port,
                server_rtp_port,
                server_rtp_port.saturating_add(1)
            )
        }
    }
}

/// Called on PLAY/TEARDOWN so a real AES67 sender (NSP nsp_aes67) can start/stop.
pub trait PlaybackHook: Send + Sync {
    fn on_play(&self, _session: &RtspSession, _transport: &SetupTransport) {}
    fn on_teardown(&self, _session_id: u8) {}
}

/// Hook that does nothing: the prototype only serves SDP.
pub struct NoopPlayback;

impl PlaybackHook for NoopPlayback {}

/// Parse a SETUP Transport header value.
pub fn parse_transport(value: &str) -> Option<SetupTransport> {
    let multicast = value.to_ascii_lowercase().contains("multicast");
    let mut t = SetupTransport {
        multicast,
        ..Default::default()
    };
    for part in value.split(';').skip(1) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();
        match key.as_str() {
            "client_port" | "port" => {
                let mut it = val.split('-');
                if let Some(p) = it.next().and_then(|p| p.parse::<u16>().ok()) {
                    t.rtp_port = p;
                }
                t.rtcp_port = it
                    .next()
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or_else(|| t.rtp_port.saturating_add(1));
            }
            "destination" => t.destination = val.parse().ok(),
            "ttl" => t.ttl = val.parse().ok(),
            _ => {}
        }
    }
    Some(t)
}

/// Map an RTSP path onto a session.
///
/// Accepts /by-name/<session>, /by-id/<n>, a bare /<session>, / or * (first
/// session), and the aes67-linux-daemon habit of embedding the node id in the
/// instance name ("<node id> <session name>").
pub fn resolve<'a>(sessions: &'a [RtspSession], path: &str) -> Option<&'a RtspSession> {
    let path = path.trim();
    if path.is_empty() || path == "/" || path == "*" {
        return sessions.first();
    }
    let p = path.trim_start_matches('/');
    if let Some(rest) = p.strip_prefix("by-id/") {
        let id: u8 = rest.trim_end_matches('/').parse().ok()?;
        return sessions.iter().find(|s| s.id == id);
    }
    if let Some(rest) = p.strip_prefix("by-name/") {
        let name = rest.trim_end_matches('/');
        // aes67-linux-daemon publishes the DNS-SD instance as
        // "<node id> <session name>" and asks for that full instance on the path,
        // so accept any name that ends with a known session name.
        return sessions.iter().find(|s| {
            s.name == name || name.ends_with(&format!(" {}", s.name))
        });
    }
    sessions.iter().find(|s| s.name == p)
}

/// Server configuration.
#[derive(Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub node_id: String,
    pub sessions: Vec<RtspSession>,
    pub playback: Arc<dyn PlaybackHook>,
}

impl ServerConfig {
    pub fn public_methods() -> &'static str {
        "OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER, SET_PARAMETER, ANNOUNCE"
    }
}

pub struct RtspServer {
    cfg: ServerConfig,
    announce_tx: broadcast::Sender<Arc<String>>,
}

impl RtspServer {
    pub fn new(cfg: ServerConfig) -> Self {
        let (announce_tx, _) = broadcast::channel(16);
        Self { cfg, announce_tx }
    }

    /// Push an updated SDP document to every connected DESCRIBE client
    /// (RAVENNA server-initiated ANNOUNCE, Operating Principles 3.4.1).
    pub fn publish_sdp_update(&self, sdp: impl Into<String>) {
        let _ = self.announce_tx.send(Arc::new(sdp.into()));
    }

    pub async fn serve(self) -> Result<()> {
        let listener = TcpListener::bind(self.cfg.bind)
            .await
            .with_context(|| format!("bind RTSP listener on {}", self.cfg.bind))?;
        info!("RTSP: listening on {}", self.cfg.bind);
        let cfg = Arc::new(self.cfg);
        loop {
            let (stream, peer) = listener.accept().await?;
            let cfg = cfg.clone();
            let tx = self.announce_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, peer, cfg, tx).await {
                    debug!("RTSP: connection from {peer} ended: {e:#}");
                }
            });
        }
    }
}

struct ConnState {
    described: Option<RtspSession>,
    transport: Option<SetupTransport>,
    server_rtp_port: u16,
    announce_cseq: u32,
}

impl ConnState {
    fn new() -> Self {
        Self {
            described: None,
            transport: None,
            server_rtp_port: reserve_rtp_port(),
            announce_cseq: 1,
        }
    }
}

/// Reserve an ephemeral UDP port for the server side of a SETUP.
///
/// The prototype keeps no socket: a real sender (nsp_aes67) owns the RTP socket
/// and must be asked for its actual port instead.
fn reserve_rtp_port() -> u16 {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap_or(0)
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    cfg: Arc<ServerConfig>,
    announce_tx: broadcast::Sender<Arc<String>>,
) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let mut announce_rx = announce_tx.subscribe();
    let mut state = ConnState::new();
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let n = read?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
                loop {
                    match parse_request(&buf) {
                        Ok(Some((req, consumed))) => {
                            buf.drain(..consumed);
                            handle_request(&req, &cfg, &mut state, &mut writer).await?;
                        }
                        Ok(None) => break,
                        Err(e) => {
                            warn!("RTSP: malformed request from {peer}: {e}");
                            writer.write_all(&RtspResponse::new(400).serialize()).await?;
                            return Ok(());
                        }
                    }
                }
            }
            update = announce_rx.recv() => {
                if let Ok(sdp) = update {
                    if let Some(session) = &state.described {
                        let announce = format!(
                            "ANNOUNCE rtsp://{}/by-name/{} {RTSP_VERSION}\r\nCSeq: {}\r\nUser-Agent: ravenna-discovery\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
                            cfg.bind,
                            session.name,
                            state.announce_cseq,
                            sdp.len(),
                            sdp
                        );
                        state.announce_cseq += 1;
                        writer.write_all(announce.as_bytes()).await?;
                        info!("RTSP: ANNOUNCE pushed to {peer} for session {}", session.name);
                    }
                }
            }
        }
    }
}

async fn handle_request(
    req: &RtspRequest,
    cfg: &ServerConfig,
    state: &mut ConnState,
    writer: &mut OwnedWriteHalf,
) -> Result<()> {
    if req.is_response() {
        // Client acknowledgement of a server-initiated ANNOUNCE.
        return Ok(());
    }
    let cseq = req.cseq();
    let path = req.path();
    debug!("RTSP: {} {}", req.method, path);
    let resp: RtspResponse = match req.method.as_str() {
        "OPTIONS" => RtspResponse::ok().with_header("Public", ServerConfig::public_methods()),
        "DESCRIBE" => match resolve(&cfg.sessions, &path) {
            Some(session) => {
                let sdp = session.sdp.sdp();
                let name = session.name.clone();
                state.described = Some(session.clone());
                RtspResponse::ok()
                    .with_header("Content-Base", format!("rtsp://{}/by-name/{name}/", cfg.bind))
                    .with_body("application/sdp", sdp.into_bytes())
            }
            None => {
                // RAVENNA Operating Principles 3.4.1: reply NOT FOUND but keep the
                // connection alive so the SDP can be pushed once it exists.
                RtspResponse::new(404)
            }
        },
        "SETUP" => {
            let transport = req
                .header("transport")
                .and_then(parse_transport)
                .unwrap_or_default();
            let ssrc = 0x4E53_5000u32;
            let session_id = resolve(&cfg.sessions, &path).map(|s| s.id).unwrap_or(0);
            let rendered = transport.render(state.server_rtp_port, ssrc);
            state.transport = Some(transport);
            RtspResponse::ok()
                .with_header("Transport", rendered)
                .with_header("Session", session_id.to_string())
        }
        "PLAY" => {
            if let (Some(session), Some(transport)) = (&state.described, &state.transport) {
                cfg.playback.on_play(session, transport);
            }
            let session_id = state.described.as_ref().map(|s| s.id).unwrap_or(0);
            let url = format!("rtsp://{}{}", cfg.bind, path);
            RtspResponse::ok()
                .with_header("Session", session_id.to_string())
                .with_header("RTP-Info", format!("url={url};seq=0;rtptime=0"))
        }
        "TEARDOWN" => {
            if let Some(session) = &state.described {
                cfg.playback.on_teardown(session.id);
            }
            state.described = None;
            state.transport = None;
            RtspResponse::ok()
        }
        "GET_PARAMETER" | "SET_PARAMETER" | "ANNOUNCE" => RtspResponse::ok(),
        other => {
            warn!("RTSP: unsupported method {other}");
            RtspResponse::new(405).with_header("Allow", ServerConfig::public_methods())
        }
    };
    writer.write_all(&resp.with_cseq(cseq).serialize()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::{SessionConfig, SessionSdp, StreamConfig, TsRefClk};
    use std::sync::Arc;

    fn session(name: &str, id: u8) -> RtspSession {
        let cfg = SessionConfig {
            session_name: name.to_string(),
            description: None,
            origin_addr: "192.168.1.10".parse().unwrap(),
            session_id: 1,
            session_version: 0,
            clock_domain: None,
            ts_refclk: TsRefClk::Local,
            streams: vec![StreamConfig {
                dest: "239.69.67.1".parse().unwrap(),
                port: 5004,
                ttl: 1,
                payload_type: 97,
                encoding: crate::sdp::Encoding::L24,
                sample_rate: 48000,
                channels: 2,
                ptime_ms: 1.0,
                ssrc: 1,
                sendonly: true,
                media_clock_direct: true,
                sync_time: None,
                frame_count: true,
            }],
        };
        RtspSession {
            id,
            name: name.to_string(),
            sdp: Arc::new(SessionSdp(cfg)),
        }
    }

    #[test]
    fn resolves_sessions_by_name_and_id() {
        let sessions = vec![session("Line 1", 1), session("Line 2", 2)];
        assert_eq!(resolve(&sessions, "/by-name/Line 2").unwrap().id, 2);
        assert_eq!(resolve(&sessions, "/by-id/1").unwrap().name, "Line 1");
        assert_eq!(resolve(&sessions, "/Line 1").unwrap().id, 1);
        // aes67-linux-daemon style: node id embedded in the instance name.
        assert_eq!(resolve(&sessions, "/by-name/AES67Bridge 0001 Line 2").unwrap().id, 2);
        assert!(resolve(&sessions, "/by-name/nope").is_none());
    }

    #[test]
    fn parses_unicast_transport() {
        let t = parse_transport("RTP/AVP;unicast;client_port=5000-5001").unwrap();
        assert!(!t.multicast);
        assert_eq!(t.rtp_port, 5000);
        assert_eq!(t.rtcp_port, 5001);
    }

    #[test]
    fn parses_multicast_transport() {
        let t = parse_transport("RTP/AVP;multicast;destination=239.69.67.1;port=5004;ttl=2").unwrap();
        assert!(t.multicast);
        assert_eq!(t.rtp_port, 5004);
        assert_eq!(t.destination.unwrap().to_string(), "239.69.67.1");
        assert_eq!(t.ttl, Some(2));
    }
}
