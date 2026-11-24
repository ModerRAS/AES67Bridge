//! aes67-feed: synthetic PCM source for host/device tests.
//!
//! Mimics the HAL tee: writes live.meta, opens live.pcm (FIFO or regular file)
//! and writes the raw PCM at real time in fixed chunks, so the daemon sees a
//! live-rate stream with a real stream_id.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use aes67d::{logln, platform};

const USAGE: &str = "aes67-feed --file RAW --fifo PATH --meta PATH [--rate 48000] [--ch 2] \
[--fmt s16|s24] [--chunk-ms 5] [--stream-id 9001] [--layout 5.1] [--seconds N] [--hold S]";

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    let (Some(file_path), Some(fifo_path), Some(meta_path)) = (
        arg_value(&args, "--file"),
        arg_value(&args, "--fifo"),
        arg_value(&args, "--meta"),
    ) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let rate: u32 = arg_value(&args, "--rate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(48_000);
    let channels: usize = arg_value(&args, "--ch")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let fmt = arg_value(&args, "--fmt").unwrap_or_else(|| "s16".to_string());
    let bytes_per_sample = match fmt.as_str() {
        "s16" | "1" => 2usize,
        "s24" | "6" => 3,
        other => {
            eprintln!("unsupported --fmt {other} (s16|s24)");
            std::process::exit(2);
        }
    };
    let chunk_ms: u32 = arg_value(&args, "--chunk-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let stream_id: i64 = arg_value(&args, "--stream-id")
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);
    let seconds: f64 = arg_value(&args, "--seconds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let hold: f64 = arg_value(&args, "--hold")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    // Optional source layout written into live.meta so the daemon can
    // intersect it with the configured target layout instead of guessing.
    let source_layout = arg_value(&args, "--layout").filter(|v| !v.trim().is_empty());
    let data = match std::fs::read(&file_path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("ERROR read {file_path}: {err}");
            std::process::exit(2);
        }
    };
    let frame_bytes = channels * bytes_per_sample;
    let total_frames = data.len() / frame_bytes;
    if total_frames == 0 {
        eprintln!("ERROR {file_path} holds no complete {channels}ch/{bytes_per_sample}B frames");
        std::process::exit(2);
    }
    let mut frame_bytes_data = data;
    frame_bytes_data.truncate(total_frames * frame_bytes);
    platform::init_timer_resolution();
    platform::install_stop_handlers();
    write_meta(
        &meta_path,
        stream_id,
        bytes_per_sample,
        rate,
        channels,
        source_layout.as_deref(),
        0,
        0,
    );
    logln!(
        "FEED start sid={stream_id} rate={rate} ch={channels} fmt={fmt} frames={total_frames} chunk_ms={chunk_ms} fifo={fifo_path}"
    );
    let mut fifo = match open_writer(&fifo_path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("ERROR open {fifo_path}: {err}");
            std::process::exit(2);
        }
    };
    let chunk_frames = ((u64::from(rate) * u64::from(chunk_ms)) / 1000).max(1) as usize;
    let period_ns = i64::from(chunk_ms) * 1_000_000;
    let start = platform::now_ns();
    let mut frame_index = 0usize;
    let mut chunk_index = 0i64;
    let mut written = 0u64;
    let mut dropped = 0u64;
    while frame_index < total_frames && !platform::stop_requested() {
        let due = start + chunk_index * period_ns;
        platform::sleep_until(due);
        if seconds > 0.0 && (platform::now_ns() - start) as f64 / 1e9 >= seconds {
            break;
        }
        let frames = chunk_frames.min(total_frames - frame_index);
        let begin = frame_index * frame_bytes;
        let end = begin + frames * frame_bytes;
        match fifo.write_all(&frame_bytes_data[begin..end]) {
            Ok(()) => {
                written += (end - begin) as u64;
                frame_index += frames;
            }
            Err(err) => {
                dropped += (end - begin) as u64;
                logln!("FEED write failed: {err} (dropping chunk)");
                frame_index += frames;
            }
        }
        chunk_index += 1;
        if chunk_index % 20 == 0 {
            write_meta(
                &meta_path,
                stream_id,
                bytes_per_sample,
                rate,
                channels,
                source_layout.as_deref(),
                written,
                dropped,
            );
        }
    }
    write_meta(
        &meta_path,
        stream_id,
        bytes_per_sample,
        rate,
        channels,
        source_layout.as_deref(),
        written,
        dropped,
    );
    logln!(
        "FEED done frames={} bytes={} dropped={}",
        frame_index,
        written,
        dropped
    );
    if hold > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(hold));
    }
    // Dropping the writer closes the FIFO write end: daemon sees EOF and flushes.
    drop(fifo);
    std::thread::sleep(Duration::from_millis(100));
}

fn open_writer(path: &str) -> std::io::Result<std::fs::File> {
    // A FIFO blocks in open until the daemon has its read end; a regular file is
    // truncated first so the daemon tails from offset zero.
    let meta = std::fs::metadata(path);
    if meta.map(|m| m.is_file()).unwrap_or(false) {
        std::fs::write(path, b"")?;
        OpenOptions::new().append(true).open(path)
    } else {
        OpenOptions::new().write(true).open(path)
    }
}

fn write_meta(
    path: &str,
    stream_id: i64,
    bytes_per_sample: usize,
    rate: u32,
    channels: usize,
    layout: Option<&str>,
    bytes: u64,
    dropped: u64,
) {
    let fmt = if bytes_per_sample == 3 {
        "0x00000006"
    } else {
        "0x00000001"
    };
    let fmt_name = if bytes_per_sample == 3 {
        "PCM_24_BIT_PACKED"
    } else {
        "PCM_16_BIT"
    };
    let now_ms = platform::now_ns() / 1_000_000;
    let layout_line = layout.map_or_else(String::new, |name| format!("layout={name}\n"));
    let body = format!(
        "stream_id={stream_id}\nfmt={fmt}\nfmt_name={fmt_name}\nrate={rate}\nch={channels}\n{layout_line}ts_ms={now_ms}\nopen_ts_ms={now_ms}\npid={}\nbytes={bytes}\ndropped={dropped}\n",
        std::process::id()
    );
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
