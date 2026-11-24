//! RAVENNA discovery-layer prototype for AES67Bridge.
//!
//! Two pieces of the RAVENNA ecosystem, both usable as a library or through the
//! ravenna-discovery binary:
//!
//! 1. DNS-SD/mDNS advertisement and browsing with the RAVENNA subtypes
//!    (_ravenna._sub._rtsp._tcp, _ravenna_session._sub._rtsp._tcp).
//! 2. A minimal RTSP 1.0 server whose DESCRIBE returns SDP for a named session
//!    (/by-name/<session>, /by-id/<n>), with RAVENNA-style ANNOUNCE pushes.
//!
//! STATUS: research + prototype only. The user paused the RAVENNA track on
//! 2026-11-24 before the ffprobe/zeroconf validation run, so nothing here has been
//! exercised against the Z9X or a real RAVENNA controller. Design decisions and
//! the known gaps are written down in
//! artifacts/aes67/ravenna-mdns-rtsp-2026-11-24.md.

pub mod config;
pub mod mdns;
pub mod rtp;
pub mod rtsp_proto;
pub mod rtsp_server;
pub mod sdp;

pub use config::{build_session, default_host_name, detect_local_ip, parse_gmid, NodeConfig};
pub use mdns::{
    Announcer, Discovered, ServiceSpec, BASE_HTTP_TYPE, BASE_RTSP_TYPE, NODE_HTTP_SUBTYPE,
    NODE_RTSP_SUBTYPE, SESSION_SUBTYPE,
};
pub use rtsp_server::{
    NoopPlayback, PlaybackHook, RtspServer, RtspSession, ServerConfig, SetupTransport,
};
pub use sdp::{Encoding, SdpProvider, SessionConfig, SessionSdp, StaticSdp, StreamConfig, TsRefClk};
