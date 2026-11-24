//! Minimal RTP sender used as a stand-in for the real AES67 sender.
//!
//! The prototype does not need media: it exists so a resumed integration (or a
//! quick ffprobe check) has an obvious place to plug the NSP sender in. This
//! module only builds RFC 3550 packets and paces silence at the SDP ptime.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::sdp::StreamConfig;

/// Build an RTP packet with the RFC 3550 fixed header (V=2, no CSRC/extension).
pub fn build_rtp_packet(
    payload_type: u8,
    marker: bool,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    out.push(0x80);
    out.push((if marker { 0x80 } else { 0 }) | (payload_type & 0x7F));
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Handle to a running sender thread.
pub struct RtpSenderHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl RtpSenderHandle {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for RtpSenderHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn a thread that sends silent RTP packets at the stream's ptime.
///
/// Timestamps advance by samples-per-packet, which is the local-clock behaviour
/// AES67 allows; do not announce PTP while this sender is in use.
pub fn spawn_silent_sender(
    sock: Arc<UdpSocket>,
    dest: SocketAddr,
    stream: StreamConfig,
) -> RtpSenderHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let join = thread::spawn(move || {
        let spp = stream.samples_per_packet().max(1);
        let payload = vec![0u8; stream.packet_bytes()];
        let interval = Duration::from_secs_f64((stream.ptime_ms / 1000.0).max(0.000_125));
        let mut seq: u16 = 0;
        let mut ts: u32 = 0;
        let mut next = Instant::now();
        while !stop_flag.load(Ordering::SeqCst) {
            let packet = build_rtp_packet(
                stream.payload_type,
                seq == 0,
                seq,
                ts,
                stream.ssrc,
                &payload,
            );
            let _ = sock.send_to(&packet, dest);
            seq = seq.wrapping_add(1);
            ts = ts.wrapping_add(spp);
            next += interval;
            let now = Instant::now();
            if next > now {
                thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    });
    RtpSenderHandle {
        stop,
        join: Some(join),
    }
}
