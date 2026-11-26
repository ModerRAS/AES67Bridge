//! AES67Bridge device-side sender daemon.
//!
//! Rust replacement for the C `aes67_rtp_sender_v4.c` (v5.3) device daemon.
//! The daemon keeps the device contract identical (live.pcm FIFO + live.meta
//! input, daemon.conf, status.json, single-instance flock, 44.1k -> 48k SRC)
//! while the AES67 transport itself is delegated to the vendored NSP
//! `nsp_aes67` crate (see `vendor/nsp_aes67`):
//!
//! * AES67 compliant packetizer: L16/L24, 125/250/500/1000 us ptime,
//!   standard multi-flow packing (<= 8 channels per flow), 1440-byte RTP
//!   payload budget, SSRC/sequence/timestamp continuity, RTP marker rule.
//! * SDP with clock reference (`ts-refclk:ptp=...` or `ts-refclk:local`),
//!   `mediaclk`, `ptime`, `source-filter`, channel labels and framecount.
//! * SAP announcements per flow, DSCP QoS (AF41 media / EF PTP).
//! * PTP slave mode (nsp_aes67 `ptp::Follower`) when `--ptp` is selected;
//!   otherwise a free-run local clock (`ts-refclk:local`, honest non-PTP
//!   operation, exactly like the C daemon).

pub mod compliance;
pub mod ctl;
pub mod keys;
pub mod bitstream;
pub mod config;
pub mod daemon;
pub mod level;
pub mod logging;
pub mod meta;
pub mod pcm;
pub mod platform;
pub mod resample;
pub mod source;
pub mod status;
pub mod trim;

pub mod sinc_poly;

pub const MAX_CH: usize = 16;
pub const OUT_RATE: u32 = 48_000;
pub const RTP_PT_L16: u8 = 96;
pub const RTP_PT_L24: u8 = 97;
pub const DEFAULT_GROUP: &str = "239.69.67.1";
pub const DEFAULT_PORT: u16 = 5067;
pub const DEFAULT_SDP: &str = "/data/haltee/aes67-v4.sdp";
pub const DEFAULT_STATUS: &str = "/data/haltee/status.json";
pub const DEFAULT_META: &str = "/data/haltee/live.meta";
pub const DEFAULT_FIFO: &str = "/data/haltee/live.pcm";
pub const DEFAULT_CONF: &str = "/data/haltee/daemon.conf";
pub const DEFAULT_LOCK: &str = "/data/local/tmp/aes67-daemon.lock";
pub const DEFAULT_MTU: usize = 1500;
/// AES67 RTP payload budget (AES67-2023: 1440 bytes incl. nothing but media).
pub const MAX_RTP_PAYLOAD: usize = 1440;
/// Local IPv4 advertised in status.json (set when the first RTP session starts).
pub static LOCAL_IP: std::sync::OnceLock<std::net::Ipv4Addr> = std::sync::OnceLock::new();