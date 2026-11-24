//! /data/haltee/live.meta parser. Mirrors the C daemon:
//! key=value lines, `strstr` matching, 250 ms polling, fmt code / fmt_name fallback.

use crate::pcm::SampleFmt;

#[derive(Clone, Debug)]
pub struct MetaState {
    pub stream_id: i64,
    pub fmt: SampleFmt,
    pub channels: usize,
    pub rate: u32,
    /// Optional source layout name from the block metadata (for example
    /// `layout=5.1`). When absent the daemon infers it from the channel count.
    pub layout: Option<String>,
    pub valid: bool,
}

impl Default for MetaState {
    fn default() -> Self {
        Self {
            stream_id: 0,
            fmt: SampleFmt::S16,
            channels: 2,
            rate: 48_000,
            layout: None,
            valid: false,
        }
    }
}

fn find_int<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.find(key).map(|i| &line[i + key.len()..])
}

fn atoi(text: &str) -> i64 {
    let trimmed = text.trim_start();
    let mut end = 0;
    let bytes = trimmed.as_bytes();
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    trimmed[..end].parse::<i64>().unwrap_or(0)
}

/// Read `path` into `state`; the previous values are kept for missing keys.
pub fn parse_meta(path: &str, state: &mut MetaState) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut channels = state.channels;
    let mut rate = state.rate;
    let mut fmt = state.fmt;
    let mut stream_id = state.stream_id;
    let mut layout = state.layout.clone();
    for line in text.lines() {
        if let Some(rest) = find_int(line, "stream_id=") {
            stream_id = atoi(rest);
        } else if let Some(rest) = find_int(line, "fmt=") {
            let rest = rest.trim_start();
            let value = rest
                .strip_prefix("0x")
                .or_else(|| rest.strip_prefix("0X"))
                .map(|hex| i64::from_str_radix(hex.trim_end(), 16).unwrap_or(0))
                .unwrap_or_else(|| atoi(rest));
            if let Some(parsed) = SampleFmt::from_hal_code(value) {
                fmt = parsed;
            }
        } else if let Some(rest) = find_int(line, "fmt_name=") {
            fmt = SampleFmt::from_name(rest);
        } else if let Some(rest) = find_int(line, "rate=") {
            rate = atoi(rest).max(0) as u32;
        } else if let Some(rest) = find_int(line, "ch=") {
            channels = atoi(rest).max(0) as usize;
        } else if let Some(rest) = find_int(line, "layout=") {
            let value = rest.trim();
            layout = if value.is_empty() { None } else { Some(value.to_string()) };
        }
    }
    state.channels = channels.clamp(1, crate::MAX_CH);
    if state.rate == 0 {
        state.rate = 48_000;
    }
    state.rate = if rate == 0 { state.rate } else { rate };
    state.fmt = fmt;
    state.stream_id = stream_id;
    state.layout = layout;
    state.valid = true;
}
/// live.bits.meta from the HAL tee v5+ (raw compressed bitstream export).
#[derive(Clone, Debug, Default)]
pub struct BitsMeta {
    pub stream_id: i64,
    pub fmt: u32,
    pub fmt_name: String,
    pub codec_guess: String,
    pub rate: u32,
    pub chmask: u32,
    pub ts_ms: i64,
    pub open_ts_ms: i64,
    pub bytes: u64,
    pub dropped: u64,
}

impl BitsMeta {
    pub fn channels(&self) -> usize {
        self.chmask.count_ones().max(1) as usize
    }

    /// No real bits have been exported yet (HAL tee meta can exist before a
    /// stream starts, often with a stale/phantom AC3 format from the last
    /// playback). Such meta must not select a decoder.
    pub fn is_stale(&self) -> bool {
        self.bytes == 0
    }

    /// Vendor audio_format_t values observed on the Z9X (AOSP shifted by one):
    /// 0x09 AC3, 0x0A E_AC3, 0x0B DTS, 0x0E IEC61937, 0x0F DOLBY_TRUEHD.
    pub fn is_dolby_es(&self) -> bool {
        matches!(self.fmt & 0xFF00_0000, 0x0900_0000 | 0x0A00_0000)
            || self.codec_guess.to_ascii_lowercase().contains("ac3")
            || self.codec_guess.to_ascii_lowercase().contains("eac3")
    }

    /// TrueHD / Dolby MAT access-unit stream (HAL tee v5+ fmt 0x0E/0x0F or codec_guess).
    pub fn is_truehd(&self) -> bool {
        matches!(self.fmt & 0xFF00_0000, 0x0E00_0000 | 0x0F00_0000)
            || self.codec_guess.to_ascii_lowercase().contains("truehd")
    }

    pub fn label(&self) -> String {
        let raw = self.fmt_name.trim();
        if !raw.is_empty() {
            return raw.to_string();
        }
        match self.fmt & 0xFF00_0000 {
            0x0900_0000 => "AC3".to_string(),
            0x0A00_0000 => "E_AC3".to_string(),
            0x0F00_0000 => "DOLBY_TRUEHD".to_string(),
            0x0E00_0000 => "IEC61937".to_string(),
            _ => format!("0x{:08x}", self.fmt),
        }
    }
}

fn parse_hex_or_dec(text: &str) -> u32 {
    let text = text.trim();
    let hex = text.trim_start_matches("0x").trim_start_matches("0X");
    if text.starts_with("0x") || text.starts_with("0X") {
        u32::from_str_radix(hex, 16).unwrap_or(0)
    } else {
        text.parse().unwrap_or(0)
    }
}

/// Parse live.bits.meta. Returns None when the file does not exist yet.
pub fn parse_bits_meta(path: &str) -> Option<BitsMeta> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut meta = BitsMeta::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "stream_id" => meta.stream_id = value.parse().unwrap_or(0),
            "fmt" => meta.fmt = parse_hex_or_dec(value),
            "fmt_name" => meta.fmt_name = value.to_string(),
            "codec_guess" => meta.codec_guess = value.to_string(),
            "rate" => meta.rate = value.parse().unwrap_or(0),
            "chmask" => meta.chmask = parse_hex_or_dec(value),
            "ts_ms" => meta.ts_ms = value.parse().unwrap_or(0),
            "open_ts_ms" => meta.open_ts_ms = value.parse().unwrap_or(0),
            "bytes" => meta.bytes = value.parse().unwrap_or(0),
            "dropped" => meta.dropped = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    if meta.rate == 0 {
        meta.rate = 48_000;
    }
    Some(meta)
}
