//! /data/haltee/status.json writer. The base schema is the C daemon's v5.3
//! schema (UI compatibility); the Rust daemon appends AES67 metadata that the
//! C contract never had (flows[], encoding, sap, ptp, silent/late/bytes).

use std::fmt::Write as _;
use std::io;

use crate::platform;

pub struct FlowStatus {
    pub index: usize,
    pub offset: usize,
    pub channels: usize,
    pub dest: String,
    pub ssrc: u32,
    pub payload_type: u8,
}

/// Optional TrueHD/dolby-pipeline diagnostics (codec=truehd / auto).
pub struct BitsStatus<'a> {
    pub object_count: usize,
    pub decode_render_ms_p50: f64,
    pub decode_render_ms_p99: f64,
    pub decode_errors: u64,
    pub extract_errors: u64,
    pub _marker: std::marker::PhantomData<&'a ()>,
}

pub struct Snapshot<'a> {
    pub mode: &'a str,
    pub group: &'a str,
    pub port: u16,
    pub maxch: usize,
    /// Speaker layout name for the UI level meters (e.g. "7.1.2").
    pub layout: &'a str,
    pub codec: &'a str,
    pub stream_id: i64,
    pub fmt_name: &'a str,
    pub rate: u32,
    pub channels: usize,
    pub packets: u64,
    pub send_err: u64,
    pub bytes_sent: u64,
    pub silent: u64,
    pub late: u64,
    pub dropped: u64,
    pub ssrc: u32,
    pub payload_type: u8,
    pub dest: String,
    pub encoding: &'a str,
    /// Number of RTP flows in the current session (UI "流数").
    pub flow_count: usize,
    /// "standard" = AES67 multi-flow packing; "legacy" = C v5.3 single-flow
    /// (RTP fragmentation or --no-split extension).
    pub split_mode: &'a str,
    pub flows: &'a [FlowStatus],
    pub rs_in: u64,
    pub rs_out: u64,
    pub levels: &'a [f64],
    pub ptime_us: u32,
    pub split: bool,
    pub ring_fill: usize,
    pub fifo_bytes: usize,
    pub locked: bool,
    pub timestamping: u32,
    pub offset_ns: f64,
    pub rate_ppm: f64,
    pub fit_rms_ns: f64,
    pub ptp_mode: &'a str,
    pub sap: bool,
    pub sap_interval_s: u32,
    pub bits: Option<&'a BitsStatus<'a>>,
}

pub fn render(snapshot: &Snapshot<'_>) -> String {
    let mut out = String::with_capacity(1024);
    let _ = write!(
        out,
        "{{\"ts\":{},\"mode\":\"{}\",\"hal\":\"running\",\"group\":\"{}\",\"port\":{},\"maxch\":{},\"layout\":\"{}\",\"codec\":\"{}\",\"flows\":{},\"split_mode\":\"{}\",",
        platform::realtime_ms(),
        snapshot.mode,
        snapshot.group,
        snapshot.port,
        snapshot.maxch,
        snapshot.layout,
        snapshot.codec,
        snapshot.flow_count,
        snapshot.split_mode
    );
    let _ = write!(
        out,
        "\"stream\":{{\"id\":{},\"fmt\":\"{}\",\"rate\":{},\"ch\":{},\"codec\":\"pcm\",\"encoding\":\"{}\"}},",
        snapshot.stream_id,
        snapshot.fmt_name,
        snapshot.rate,
        snapshot.channels,
        snapshot.encoding
    );
    let _ = write!(
        out,
        "\"rtp\":{{\"packets\":{},\"send_err\":{},\"bytes\":{},\"silent\":{},\"late\":{},\"dropped\":{},\"ssrc\":\"0x{:08x}\",\"pt\":{},\"dest\":\"{}\",\"ch\":{}}},",
        snapshot.packets,
        snapshot.send_err,
        snapshot.bytes_sent,
        snapshot.silent,
        snapshot.late,
        snapshot.dropped,
        snapshot.ssrc,
        snapshot.payload_type,
        snapshot.dest,
        snapshot.channels
    );
    out.push_str("\"flow_details\":[");
    for (i, flow) in snapshot.flows.iter().enumerate() {
        if i != 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"index\":{},\"offset\":{},\"ch\":{},\"dest\":\"{}\",\"ssrc\":\"0x{:08x}\",\"pt\":{},\"encoding\":\"{}\"}}",
            flow.index,
            flow.offset,
            flow.channels,
            flow.dest,
            flow.ssrc,
            flow.payload_type,
            snapshot.encoding
        );
    }
    out.push_str("],");
    let _ = write!(
        out,
        "\"src\":{{\"rs_in\":{},\"rs_out\":{}}},\"levels\":[",
        snapshot.rs_in, snapshot.rs_out
    );
    for (i, level) in snapshot.levels.iter().enumerate() {
        if i != 0 {
            out.push(',');
        }
        let _ = write!(out, "{level:.2}");
    }
    let _ = write!(
        out,
        "],\"ptime_us\":{},\"split\":{},\"ring\":{{\"fill\":{}}},\"fifo\":{{\"bytes\":{}}},",
        snapshot.ptime_us,
        u8::from(snapshot.split),
        snapshot.ring_fill,
        snapshot.fifo_bytes
    );
    let _ = write!(
        out,
        "\"ptp\":{{\"mode\":\"{}\",\"locked\":{},\"timestamping\":{},\"offset_ns\":{:.1},\"rate_ppm\":{:.3},\"fit_rms_ns\":{:.1}}},",
        snapshot.ptp_mode,
        u8::from(snapshot.locked),
        snapshot.timestamping,
        snapshot.offset_ns,
        snapshot.rate_ppm,
        snapshot.fit_rms_ns
    );
    if let Some(bits) = snapshot.bits {
        let _ = writeln!(
            out,
            "\"sap\":{{\"enabled\":{},\"interval_s\":{}}},\"bits\":{{\"object_count\":{},\"decode_render_ms_p50\":{:.3},\"decode_render_ms_p99\":{:.3},\"decode_errors\":{},\"extract_errors\":{}}}}}",
            u8::from(snapshot.sap),
            snapshot.sap_interval_s,
            bits.object_count,
            bits.decode_render_ms_p50,
            bits.decode_render_ms_p99,
            bits.decode_errors,
            bits.extract_errors
        );
    } else {
        let _ = writeln!(
            out,
            "\"sap\":{{\"enabled\":{},\"interval_s\":{}}}}}",
            u8::from(snapshot.sap),
            snapshot.sap_interval_s
        );
    }
    out
}


// ── UNIX-socket status channel ──────────────────────────────────────────────
// The TV app reads status from an abstract LOCAL socket (@aes67bridge.status)
// instead of polling a JSON file. Each publish() writes the current snapshot to
// all clients that connected since the previous publish, then closes them.
// The JSON file writer below is kept for the legacy C engine and for tests.

/// Abstract LOCAL socket name used by the Rust daemon and the TV app.
pub const SOCKET_NAME: &str = "aes67bridge.status";

// The channel is Linux/Android-only: abstract LOCAL sockets have no equivalent
// on Windows, and host builds exist so `cargo test` can run. Elsewhere the
// daemon logs the unsupported bind and keeps writing the JSON status file.
#[cfg(unix)]
mod socket {
    use super::{render, Snapshot};
    use std::io;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::net::UnixListener;
    use std::sync::OnceLock;

    static STATUS_SOCKET: OnceLock<Option<UnixListener>> = OnceLock::new();

    /// Bind an abstract LOCAL socket (Linux/Android `sun_path[0] = 0`).
    fn bind_abstract(name: &str) -> io::Result<UnixListener> {
        let bytes = name.as_bytes();
        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            if bytes.len() + 1 > addr.sun_path.len() {
                libc::close(fd);
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket name too long"));
            }
            for (i, byte) in bytes.iter().enumerate() {
                addr.sun_path[i + 1] = *byte as libc::c_char;
            }
            let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
            if libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) != 0 {
                let err = io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            if libc::listen(fd, 8) != 0 {
                let err = io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            Ok(UnixListener::from_raw_fd(fd))
        }
    }

    /// Bind the abstract status socket. Call once, after the instance lock is held.
    pub fn init_socket(name: &str) -> io::Result<()> {
        let listener = bind_abstract(name)?;
        listener.set_nonblocking(true)?;
        let _ = STATUS_SOCKET.set(Some(listener));
        Ok(())
    }

    /// Publish one snapshot to every status client connected right now.
    pub fn publish(snapshot: &Snapshot<'_>) {
        let body = render(snapshot);
        let Some(Some(listener)) = STATUS_SOCKET.get() else {
            return;
        };
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    use std::io::Write as _;
                    let _ = stream.write_all(body.as_bytes());
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }
}

#[cfg(not(unix))]
mod socket {
    use super::Snapshot;
    use std::io;

    /// No abstract LOCAL sockets off unix; the JSON file writer stays in charge.
    pub fn init_socket(_name: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "status socket is unix-only",
        ))
    }

    pub fn publish(_snapshot: &Snapshot<'_>) {}
}

pub use socket::{init_socket, publish};

/// Atomic 0644 write: temp file + rename, exactly like the C daemon.
pub fn write(path: &str, snapshot: &Snapshot<'_>) -> io::Result<()> {
    let body = render(snapshot);
    let tmp = format!("{path}.tmp");
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(body.as_bytes())?;
    }
    platform::chmod_644(&tmp);
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        err
    })?;
    platform::chmod_644(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_layout_for_the_ui_level_meters() {
        let snapshot = Snapshot {
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
            packets: 10,
            send_err: 0,
            bytes_sent: 1440,
            silent: 0,
            late: 0,
            dropped: 0,
            ssrc: 0x4e53_5034,
            payload_type: 97,
            dest: "239.69.67.1:5067".to_string(),
            encoding: "L24",
            flow_count: 2,
            split_mode: "standard",
            flows: &[],
            rs_in: 0,
            rs_out: 0,
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
        let text = render(&snapshot);
        assert!(text.contains("\"layout\":\"7.1.2\""), "{text}");
        assert!(text.contains("\"maxch\":10"), "{text}");
        // UI contract: numeric flow count + wire mode, and the old split flag
        // stays for compatibility.
        assert!(text.contains("\"flows\":2"), "{text}");
        assert!(text.contains("\"split_mode\":\"standard\""), "{text}");
        assert!(text.contains("\"split\":1"), "{text}");
        assert!(text.contains("\"flow_details\":["), "{text}");
    }
}