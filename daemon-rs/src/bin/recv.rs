//! aes67-recv: AES67 RTP receiver + verifier used by the A/B tests.
//!
//! Receives one RTP flow, reassembles ptime groups (handles the C daemon's
//! same-timestamp split fragments as well as the NSP crate's one-packet-per-
//! interval mode), writes the concatenated payload as a raw network-order dump
//! (for byte-exact A/B comparison), an optional WAV, and a JSON statistics file.

use std::io::Write;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use aes67d::logln;

const USAGE: &str = "aes67-recv [--sdp FILE] [--iface IP] [--group G] [--port P] [--ch N] \
[--rate HZ] [--encoding l24|l16] [--ptime-us US] [--seconds S] [--out WAV] \
[--raw RAW] [--json JSON] [--ts-map FILE] [--arrival-log FILE] \
[--expect-ssrc HEX] [--keep-leading-silence]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    L16,
    L24,
}

impl Encoding {
    fn bytes(self) -> usize {
        match self {
            Encoding::L16 => 2,
            Encoding::L24 => 3,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Encoding::L16 => "L16",
            Encoding::L24 => "L24",
        }
    }
}

struct Format {
    group: Ipv4Addr,
    port: u16,
    channels: usize,
    rate: u32,
    encoding: Encoding,
    ptime_us: u32,
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn parse_sdp(path: &str) -> Result<Format, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("read SDP {path}: {err}"))?;
    let mut format = Format {
        group: Ipv4Addr::new(239, 69, 67, 1),
        port: 5067,
        channels: 2,
        rate: 48_000,
        encoding: Encoding::L24,
        ptime_us: 1_000,
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("c=IN IP4 ") {
            let ip = rest.split('/').next().unwrap_or(rest).trim();
            if let Ok(parsed) = ip.parse() {
                format.group = parsed;
            }
        } else if let Some(rest) = line.strip_prefix("m=audio ") {
            let mut parts = rest.split_whitespace();
            if let Some(port) = parts.next().and_then(|p| p.parse().ok()) {
                format.port = port;
            }
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            let Some((_pt, codec)) = rest.split_once(' ') else {
                continue;
            };
            let mut parts = codec.split('/');
            let name = parts.next().unwrap_or("L24").trim().to_ascii_uppercase();
            format.encoding = if name.contains("16") {
                Encoding::L16
            } else {
                Encoding::L24
            };
            if let Some(rate) = parts.next().and_then(|v| v.parse().ok()) {
                format.rate = rate;
            }
            if let Some(channels) = parts.next().and_then(|v| v.parse().ok()) {
                format.channels = channels;
            }
        } else if let Some(rest) = line.strip_prefix("a=ptime:") {
            if let Ok(ptime_ms) = rest.trim().parse::<f64>() {
                format.ptime_us = (ptime_ms * 1000.0).round() as u32;
            }
        }
    }
    Ok(format)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    let mut format = match arg_value(&args, "--sdp") {
        Some(path) => match parse_sdp(&path) {
            Ok(format) => format,
            Err(message) => {
                eprintln!("ERROR {message}");
                std::process::exit(2);
            }
        },
        None => Format {
            group: Ipv4Addr::new(239, 69, 67, 1),
            port: 5067,
            channels: 2,
            rate: 48_000,
            encoding: Encoding::L24,
            ptime_us: 1_000,
        },
    };
    let iface: Ipv4Addr = arg_value(&args, "--iface")
        .and_then(|v| v.parse().ok())
        .unwrap_or(Ipv4Addr::UNSPECIFIED);
    if let Some(value) = arg_value(&args, "--group").and_then(|v| v.parse().ok()) {
        format.group = value;
    }
    if let Some(value) = arg_value(&args, "--port").and_then(|v| v.parse().ok()) {
        format.port = value;
    }
    if let Some(value) = arg_value(&args, "--ch").and_then(|v| v.parse().ok()) {
        format.channels = value;
    }
    if let Some(value) = arg_value(&args, "--rate").and_then(|v| v.parse().ok()) {
        format.rate = value;
    }
    if let Some(value) = arg_value(&args, "--encoding") {
        format.encoding = if value.to_ascii_uppercase().contains("16") {
            Encoding::L16
        } else {
            Encoding::L24
        };
    }
    if let Some(value) = arg_value(&args, "--ptime-us").and_then(|v| v.parse().ok()) {
        format.ptime_us = value;
    }
    let seconds: f64 = arg_value(&args, "--seconds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5.0);
    let keep_silence = args.iter().any(|a| a == "--keep-leading-silence");
    let max_frames: u64 = arg_value(&args, "--max-frames")
        .and_then(|v| v.parse().ok())
        .unwrap_or(u64::MAX);
    let expected_ssrc = arg_value(&args, "--expect-ssrc").and_then(|value| {
        let hex = value.trim_start_matches("0x").trim_start_matches("0X");
        u32::from_str_radix(hex, 16).ok()
    });
    let raw_path = arg_value(&args, "--raw");
    let wav_path = arg_value(&args, "--out");
    let json_path = arg_value(&args, "--json");
    let ts_map_path = arg_value(&args, "--ts-map");
    let arrival_log_path = arg_value(&args, "--arrival-log");

    let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, format.port)) {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!("ERROR bind UDP {}: {err}", format.port);
            std::process::exit(2);
        }
    };
    if format.group.is_multicast() {
        if let Err(err) = socket.join_multicast_v4(&format.group, &iface) {
            eprintln!("ERROR join {} on {iface}: {err}", format.group);
            std::process::exit(2);
        }
    }
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    logln!(
        "RECV start group={} port={} sdp_ch={} rate={} encoding={} ptime_us={} seconds={seconds}",
        format.group,
        format.port,
        format.channels,
        format.rate,
        format.encoding.name(),
        format.ptime_us
    );
    let samples = ((u64::from(format.rate) * u64::from(format.ptime_us)) / 1_000_000).max(1) as usize;
    let frame_bytes = format.channels * format.encoding.bytes();
    let expected_payload = samples * frame_bytes;

    let started = Instant::now();
    let mut buffer = vec![0u8; 65_536];
    let mut groups: Vec<Vec<u8>> = Vec::new();
    let mut group_ts: Vec<u32> = Vec::new();
    let mut group_seq: Vec<u16> = Vec::new();
    // (sequence, RTP timestamp, arrival offset ns) for packet-interval A/B.
    let mut arrivals: Vec<(u16, u32, u64)> = Vec::new();
    let epoch = Instant::now();
    let mut pending: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    let mut packets = 0u64;
    let mut bytes = 0u64;
    let mut lost = 0u64;
    let mut reordered = 0u64;
    let mut duplicates = 0u64;
    let mut ssrcs: Vec<u32> = Vec::new();
    let mut first_seq: Option<u16> = None;
    let mut last_seq: Option<u16> = None;
    let mut expected_seq: Option<u16> = None;
    let mut first_ts: Option<u32> = None;
    let mut last_ts: Option<u32> = None;
    let mut ts_jumps = 0u64;
    let mut payload_size_errors = 0u64;
    let mut marker_packets = 0u64;
    let mut frames_seen = 0u64;
    let mut ssrc_changes = 0u64;
    while started.elapsed().as_secs_f64() < seconds {
        match socket.recv_from(&mut buffer) {
            Ok((size, _peer)) => {
                if size < 12 {
                    continue;
                }
                if buffer[0] != 0x80 {
                    continue;
                }
                let pt = buffer[1] & 0x7f;
                let marker = buffer[1] & 0x80 != 0;
                let seq = u16::from_be_bytes([buffer[2], buffer[3]]);
                let ts = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
                let ssrc = u32::from_be_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]);
                if let Some(expected) = expected_ssrc {
                    if ssrc != expected {
                        continue;
                    }
                }
                if !ssrcs.contains(&ssrc) {
                    if !ssrcs.is_empty() {
                        // New RTP stream epoch (daemon restarted the session):
                        // discard everything captured from the previous SSRC.
                        ssrc_changes += 1;
                        groups.clear();
                        group_ts.clear();
                        group_seq.clear();
                        pending.clear();
                        arrivals.clear();
                        packets = 0;
                        lost = 0;
                        reordered = 0;
                        duplicates = 0;
                        first_seq = None;
                        last_seq = None;
                        expected_seq = None;
                        first_ts = None;
                        last_ts = None;
                        ts_jumps = 0;
                        payload_size_errors = 0;
                        marker_packets = 0;
                        frames_seen = 0;
                    }
                    ssrcs.push(ssrc);
                }
                packets += 1;
                bytes += (size - 12) as u64;
                if marker {
                    marker_packets += 1;
                }
                if let Some(expected) = expected_seq {
                    let diff = seq.wrapping_sub(expected);
                    if diff == 0 {
                        expected_seq = Some(seq.wrapping_add(1));
                    } else if diff < 0x8000 {
                        lost += u64::from(diff);
                        expected_seq = Some(seq.wrapping_add(1));
                    } else {
                        // Older than expected: duplicate or out-of-order arrival.
                        if pending.contains_key(&ts) {
                            duplicates += 1;
                        } else {
                            reordered += 1;
                        }
                    }
                } else {
                    first_seq = Some(seq);
                    expected_seq = Some(seq.wrapping_add(1));
                }
                arrivals.push((seq, ts, epoch.elapsed().as_nanos() as u64));
                if first_ts.is_none() {
                    first_ts = Some(ts);
                }
                if let Some(prev) = last_ts {
                    // Every fragment of one interval repeats the timestamp, so
                    // only a new interval can jump the media clock.
                    if ts != prev {
                        let expected_ts = prev.wrapping_add(samples as u32);
                        if ts != expected_ts {
                            ts_jumps += 1;
                        }
                        last_ts = Some(ts);
                    }
                } else {
                    last_ts = Some(ts);
                }
                last_seq = Some(seq);
                let payload = &buffer[12..size];
                // One interval is either a single full packet (AES67 media
                // profile) or every fragment sharing one timestamp, flushed by
                // the marker bit (C v5.3 MTU fragmentation). The assembled
                // interval, not the fragment, is what the size check targets.
                let entry = pending.entry(ts).or_default();
                entry.extend_from_slice(payload);
                if marker || entry.len() >= expected_payload {
                    if let Some(full) = pending.remove(&ts) {
                        if full.len() != expected_payload {
                            payload_size_errors += 1;
                        }
                        frames_seen += (full.len() / frame_bytes) as u64;
                        groups.push(full);
                        group_ts.push(ts);
                        group_seq.push(seq);
                    }
                }
                let _ = pt;
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) => {
                logln!("RECV socket error: {err}");
                break;
            }
        }
    }
    // Fragments still open (should not happen for one-packet-per-interval flows).
    let mut leftover: Vec<(u32, Vec<u8>)> = pending.into_iter().collect();
    leftover.sort_by_key(|(_, payload)| payload.len());
    for (ts, payload) in leftover {
        groups.push(payload);
        group_ts.push(ts);
        group_seq.push(last_seq.unwrap_or(0));
    }

    let mut leading_silent = 0usize;
    if !keep_silence {
        while leading_silent < groups.len() && groups[leading_silent].iter().all(|&b| b == 0) {
            leading_silent += 1;
        }
    }
    let selected = &groups[leading_silent..];
    let selected_ts = &group_ts[leading_silent..];
    let selected_seq = &group_seq[leading_silent..];
    let mut payload = Vec::with_capacity(selected.iter().map(Vec::len).sum());
    for group in selected {
        if (payload.len() / frame_bytes) as u64 >= max_frames && max_frames != u64::MAX {
            break;
        }
        payload.extend_from_slice(group);
    }
    let frames = (payload.len() / frame_bytes) as u64;
    // Per-channel RMS in 24-bit full scale.
    let mut sumsq = vec![0.0f64; format.channels];
    let mut peak = 0i32;
    for frame in payload.chunks_exact(frame_bytes) {
        for c in 0..format.channels {
            let value = match format.encoding {
                Encoding::L16 => {
                    let raw = i16::from_be_bytes([frame[c * 2], frame[c * 2 + 1]]);
                    i32::from(raw) << 8
                }
                Encoding::L24 => {
                    let raw = i32::from(frame[c * 3])
                        | (i32::from(frame[c * 3 + 1]) << 8)
                        | (i32::from(frame[c * 3 + 2]) << 16);
                    if raw & 0x0080_0000 != 0 {
                        raw | !0x00ff_ffff
                    } else {
                        raw
                    }
                }
            };
            sumsq[c] += f64::from(value) * f64::from(value);
            peak = peak.max(value.abs());
        }
    }
    let mut rms_dbfs = String::from("[");
    for (c, sum) in sumsq.iter().enumerate() {
        if c != 0 {
            rms_dbfs.push(',');
        }
        let dbfs = if frames > 0 && *sum > 0.0 {
            (20.0 * ((sum / frames as f64).sqrt() / 8_388_608.0).log10()).max(-120.0)
        } else {
            -120.0
        };
        rms_dbfs.push_str(&format!("{dbfs:.2}"));
    }
    rms_dbfs.push(']');
    if let Some(path) = ts_map_path.as_deref() {
        // One line per selected interval: RTP timestamp plus payload bytes.
        // The byte offset in the raw dump lets a checker place PCM on the RTP
        // media clock, so pacer slot drops and UDP loss do not make the A/B
        // comparison fail spuriously.
        let mut text = String::with_capacity(selected.len() * 16);
        for ((seq, ts), group) in selected_seq
            .iter()
            .zip(selected_ts.iter())
            .zip(selected.iter())
        {
            text.push_str(&format!("{seq}\t{ts}\t{}\n", group.len()));
        }
        if let Err(err) = std::fs::write(path, text.as_bytes()) {
            logln!("WARN ts-map write {path}: {err}");
        }
    }
    if let Some(path) = arrival_log_path.as_deref() {
        // One line per RTP datagram: sequence, RTP timestamp, arrival offset in
        // nanoseconds from receiver start. Lets the A/B report compare packet
        // intervals between the C v5.3 and Rust daemons without a sniffer.
        let mut text = String::with_capacity(arrivals.len() * 24);
        for (seq, ts, arrival_ns) in arrivals.iter() {
            text.push_str(&format!("{seq}\t{ts}\t{arrival_ns}\n"));
        }
        if let Err(err) = std::fs::write(path, text.as_bytes()) {
            logln!("WARN arrival-log write {path}: {err}");
        }
    }
    if let Some(path) = raw_path.as_deref() {
        if let Err(err) = std::fs::write(path, &payload) {
            logln!("WARN raw write {path}: {err}");
        }
    }
    if let Some(path) = wav_path.as_deref() {
        if let Err(err) = write_wav(path, &payload, &format) {
            logln!("WARN wav write {path}: {err}");
        }
    }
    let summary = format!(
        "RECV done packets={packets} lost={lost} reordered={reordered} duplicates={duplicates} frames={frames} groups={} leading_silent={leading_silent} ts_jumps={ts_jumps} size_errors={payload_size_errors} markers={marker_packets} ssrc={:?}",
        groups.len(),
        ssrcs
    );
    logln!("{summary}");
    let ssrc_hex = ssrcs
        .first()
        .map(|value| format!("0x{value:08x}"))
        .unwrap_or_else(|| "0x00000000".to_string());
    let stats = format!(
        "{{\"packets\":{packets},\"payload_bytes\":{},\"lost\":{lost},\"reordered\":{reordered},\"duplicates\":{duplicates},\"groups\":{},\"leading_silent_groups\":{leading_silent},\"frames\":{frames},\"rate\":{},\"channels\":{},\"encoding\":\"{}\",\"ptime_us\":{},\"ssrc\":\"{ssrc_hex}\",\"ssrc_changes\":{ssrc_changes},\"seq_first\":{seq_first},\"seq_last\":{seq_last},\"first_ts\":{first_ts},\"last_ts\":{last_ts},\"ts_jumps\":{ts_jumps},\"size_errors\":{payload_size_errors},\"marker_packets\":{marker_packets},\"peak\":{peak},\"rms_dbfs\":{rms_dbfs}}}",
        payload.len(),
        groups.len(),
        format.rate,
        format.channels,
        format.encoding.name(),
        format.ptime_us,
        seq_first = first_seq.map_or(0, i64::from),
        seq_last = last_seq.map_or(0, i64::from),
        first_ts = first_ts.map_or(0, i64::from),
        last_ts = last_ts.map_or(0, i64::from),
    );
    println!("{summary}");
    if let Some(path) = json_path.as_deref() {
        if let Err(err) = std::fs::write(path, stats.as_bytes()) {
            logln!("WARN json write {path}: {err}");
        }
    }
}

fn write_wav(path: &str, payload: &[u8], format: &Format) -> std::io::Result<()> {
    let bytes_per_sample = format.encoding.bytes();
    let frame_bytes = format.channels * bytes_per_sample;
    let frames = payload.len() / frame_bytes;
    let data_len = (frames * frame_bytes) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&(format.channels as u16).to_le_bytes());
    out.extend_from_slice(&format.rate.to_le_bytes());
    out.extend_from_slice(&(format.rate * frame_bytes as u32).to_le_bytes());
    out.extend_from_slice(&(frame_bytes as u16).to_le_bytes());
    out.extend_from_slice(&((bytes_per_sample * 8) as u16).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for frame in payload.chunks_exact(frame_bytes) {
        for c in 0..format.channels {
            let sample = &frame[c * bytes_per_sample..(c + 1) * bytes_per_sample];
            for byte in sample.iter().rev() {
                out.push(*byte);
            }
        }
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(&out)
}


