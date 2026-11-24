//! live.bits -> elementary stream boundary handling.
//!
//! Phase-0 conclusion (see artifacts/aes67/roar-armv7-bench-2026-11-24.md §4):
//! the live.bits bytes for TrueHD are the TrueHD access-unit stream itself,
//! \`[4-byte AU header][AU payload]\` repeated. The 4-byte header is NOT a HAL burst wrapper and
//! NOT an IEC61937 Pa/Pb preamble: truehdd's extractor natively frames on it
//! (\`access_unit_len = (be_u16 & 0x0FFF) << 1\`, scan starts at offset 4). Removing one 4-byte
//! header per container packet breaks decoding (12 AU + 70 errors instead of 27,948 AU).
//!
//! Therefore this module **passes bytes through unchanged**. It only:
//! * classifies the codec (TrueHD / AC3 / E-AC3 / DTS) from the byte stream,
//! * exposes the TrueHD access-unit header (length / flags / 40-sample counter) for diagnostics,
//! * keeps stream statistics.
//!
//! The decoder must be prepared for an arbitrary reader start: live.bits is a byte stream whose
//! first byte may be in the middle of an access unit. truehdd re-syncs; the decode layer drops
//! AUs until the first major sync (presentation map init).

use crate::{PipelineError, Result};

/// Payload codec of the live.bits / elementary stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    TrueHd,
    Ac3,
    Eac3,
    Dts,
    Unknown,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Codec::TrueHd => "truehd",
            Codec::Ac3 => "ac3",
            Codec::Eac3 => "eac3",
            Codec::Dts => "dts",
            Codec::Unknown => "unknown",
        }
    }
}

/// TrueHD access-unit header (first four bytes of every AU in the stream):
/// `check(4) | len_words(12) | input_timing(16)`, where `len_bytes = len_words * 2` and the major
/// sync word starts at AU offset 4. `input_timing` is the 40-sample counter at 48 kHz.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TrueHdAuHeader {
    /// AU length in bytes: \`(u16_be(bytes[0..2]) & 0x0FFF) << 1\`.
    pub len_bytes: usize,
    /// High nibble of the first byte (the AU "check" field; not needed by the pipeline).
    pub flags: u8,
    /// 16-bit sample counter at bytes[2..4]; increments by 40 per 48 kHz AU.
    pub sample_counter: u16,
}

impl TrueHdAuHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            return Err(PipelineError::Bits("TrueHD AU header needs 4 bytes".into()));
        }
        let word = u16::from_be_bytes([bytes[0], bytes[1]]);
        let len_bytes = ((word & 0x0FFF) as usize) << 1;
        if len_bytes < 8 {
            return Err(PipelineError::Bits(format!(
                "implausible TrueHD AU length {len_bytes} (header {:02x?})",
                &bytes[..4]
            )));
        }
        Ok(Self {
            len_bytes,
            flags: (bytes[0] >> 4) & 0x0F,
            sample_counter: u16::from_be_bytes([bytes[2], bytes[3]]),
        })
    }
}

const TRUEHD_MAJOR_SYNC: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBA];
const TRUEHD_MINOR_SYNC: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBB];

fn find_sync(haystack: &[u8]) -> Option<usize> {
    haystack.windows(4).position(|w| w == TRUEHD_MAJOR_SYNC || w == TRUEHD_MINOR_SYNC)
}

/// Classify the stream from its first bytes.
///
/// * AC3 / E-AC3: sync word 0x0B77 (ac3/eac3 share the sync; distinguish via bsid when possible,
///   but for streaming purposes both are passed to the same extractor family; E-AC-3 is decoded by
///   the daemon side).
/// * DTS: 0x7FFE8001 family sync (\`0x7FFE80xx\` / 0x7FFE80xx masked).
/// * TrueHD: a 0xF8726FBA/FBB sync word whose preceding 4 bytes form a plausible AU header
///   (\`(be16 & 0xFFF) << 1\` covers at least the remaining bytes of the AU).
pub fn detect_codec(buf: &[u8]) -> Codec {
    if buf.len() >= 2 && u16::from_be_bytes([buf[0], buf[1]]) == 0x0B77 {
        return if buf.len() >= 6 && (buf[4] >> 3) == 16 { Codec::Eac3 } else { Codec::Ac3 };
    }
    if buf.len() >= 4 {
        let word = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if word & 0xFFFF_FF00 == 0x7FFE_8000 {
            return Codec::Dts;
        }
    }
    // TrueHD: scan for a sync word that sits 4 bytes into an access unit.
    if let Some(pos) = find_sync(buf) {
        let au_start = pos.saturating_sub(4);
        if au_start + 2 <= buf.len() {
            let word = u16::from_be_bytes([buf[au_start], buf[au_start + 1]]);
            let len = ((word & 0x0FFF) as usize) << 1;
            // The header must be able to cover the major sync it precedes.
            if len >= 8 && au_start + len >= pos + 4 {
                return Codec::TrueHd;
            }
        }
        // A bare sync word is still strong evidence of TrueHD.
        return Codec::TrueHd;
    }
    Codec::Unknown
}

/// Streaming classifier + transparent byte pass-through.
///
/// \`push\` returns the exact bytes it was given (no strip, no reordering). The separate return
/// value is kept so callers cannot accidentally treat a transformed buffer as ES.
#[derive(Default, Debug)]
pub struct EsNormalizer {
    codec: Option<Codec>,
    bytes_in: u64,
    probe: Vec<u8>,
    major_sync_offset: Option<u64>,
}

const PROBE_LIMIT: usize = 64 * 1024;

impl EsNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of live.bits bytes. Returns the same bytes to forward to the decoder.
    pub fn push<'a>(&mut self, chunk: &'a [u8]) -> &'a [u8] {
        self.bytes_in += chunk.len() as u64;
        if self.codec.is_none() {
            let remaining = PROBE_LIMIT.saturating_sub(self.probe.len());
            let take = remaining.min(chunk.len());
            self.probe.extend_from_slice(&chunk[..take]);
            if self.probe.len() >= 8 {
                let codec = detect_codec(&self.probe);
                if codec != Codec::Unknown {
                    self.codec = Some(codec);
                }
            }
            if self.major_sync_offset.is_none() {
                if let Some(pos) = self.probe.windows(4).position(|w| w == TRUEHD_MAJOR_SYNC) {
                    self.major_sync_offset = Some(pos as u64);
                }
            }
        }
        chunk
    }

    pub fn codec(&self) -> Codec {
        self.codec.unwrap_or(Codec::Unknown)
    }

    pub fn bytes_in(&self) -> u64 {
        self.bytes_in
    }

    /// Offset of the first major sync inside the probe window (None until seen / if not TrueHD).
    pub fn first_major_sync_offset(&self) -> Option<u64> {
        self.major_sync_offset
    }
}

/// Iterate TrueHD access units as byte slices (header included). Tolerant helper for tools: stops
/// at the first incomplete/invalid AU, returning what was parsed so far.
pub struct AuIter<'a> {
    bytes: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> AuIter<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0, failed: false }
    }

    pub fn position(&self) -> usize {
        self.pos
    }
}

impl<'a> Iterator for AuIter<'a> {
    type Item = Result<(&'a [u8], TrueHdAuHeader)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos + 4 > self.bytes.len() {
            return None;
        }
        let header = match TrueHdAuHeader::parse(&self.bytes[self.pos..]) {
            Ok(h) => h,
            Err(e) => {
                self.failed = true;
                return Some(Err(e));
            }
        };
        if self.pos + header.len_bytes > self.bytes.len() {
            return None; // incomplete tail (normal for a live FIFO read)
        }
        let au = &self.bytes[self.pos..self.pos + header.len_bytes];
        self.pos += header.len_bytes;
        Some(Ok((au, header)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_truehd_au(after_major_sync_len: usize, counter: u16) -> Vec<u8> {
        // AU payload: [major sync][filler]
        let payload_len = 4 + after_major_sync_len;
        let total = 4 + payload_len; // header + payload
        let mut au = vec![0u8; total];
        let word = ((total / 2) as u16) & 0x0FFF; // length in 16-bit words
        au[0..2].copy_from_slice(&(0x8000 | word).to_be_bytes());
        au[2..4].copy_from_slice(&counter.to_be_bytes());
        au[4..8].copy_from_slice(&TRUEHD_MAJOR_SYNC);
        au
    }

    #[test]
    fn detects_truehd_from_au_stream() {
        let mut stream = Vec::new();
        for i in 0..8u16 {
            stream.extend_from_slice(&make_truehd_au(40, i * 40));
        }
        let mut normalizer = EsNormalizer::new();
        let out = normalizer.push(&stream);
        assert_eq!(out, &stream[..], "pass-through must be byte identical");
        assert_eq!(normalizer.codec(), Codec::TrueHd);
        assert_eq!(normalizer.first_major_sync_offset(), Some(4));
        assert_eq!(normalizer.bytes_in(), stream.len() as u64);
    }

    #[test]
    fn detects_chunked_truehd_across_probe_limit() {
        let stream: Vec<u8> = (0..3000u16).flat_map(|i| make_truehd_au(20, i.wrapping_mul(40))).collect();
        let mut normalizer = EsNormalizer::new();
        let mut forwarded = Vec::new();
        for chunk in stream.chunks(37) {
            let out = normalizer.push(chunk);
            forwarded.extend_from_slice(out);
        }
        assert_eq!(forwarded, stream);
        assert_eq!(normalizer.codec(), Codec::TrueHd);
    }

    #[test]
    fn detects_ac3_eac3_and_dts() {
        assert_eq!(detect_codec(&[0x0B, 0x77, 0x12, 0x40]), Codec::Ac3);
        // E-AC-3: bsid lives in byte 4 bits 3..8; 16 = E-AC-3
        assert_eq!(detect_codec(&[0x0B, 0x77, 0x00, 0x00, 0x80, 0x00]), Codec::Eac3);
        assert_eq!(detect_codec(&[0x7F, 0xFE, 0x80, 0x01, 0x00]), Codec::Dts);
        assert_eq!(detect_codec(&[0x00, 0x01, 0x02, 0x03]), Codec::Unknown);
    }

    #[test]
    fn au_header_length_and_counter() {
        let au = make_truehd_au(36, 1234);
        let h = TrueHdAuHeader::parse(&au).unwrap();
        assert_eq!(au.len(), 44);
        assert_eq!(h.len_bytes, 44);
        assert_eq!(h.sample_counter, 1234);
    }

    #[test]
    fn au_iterator_walks_headers_and_stops_on_partial_tail() {
        let mut stream = Vec::new();
        for i in 0..5u16 {
            stream.extend_from_slice(&make_truehd_au(36, i * 40));
        }
        let a = make_truehd_au(36, 9999);
        stream.extend_from_slice(&a[..10]); // partial tail
        let aus: Vec<_> = AuIter::new(&stream).collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(aus.len(), 5);
        for (idx, (au, header)) in aus.iter().enumerate() {
            assert_eq!(header.sample_counter, idx as u16 * 40);
            assert_eq!(au.len(), 44);
        }
    }

    #[test]
    fn live_bits_bits19_fixture_parses_as_au_stream_when_present() {
        // Optional artifact test: the device dump is [4B header][major sync ...] and decodes as
        // 340-byte first AU. Skipped when the artifacts tree is absent (e.g. installed crate).
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../artifacts/aes67/haltee-v5/evidence/bits_19_fmt0e000000.bin");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let first = TrueHdAuHeader::parse(&bytes).unwrap();
        assert_eq!(first.len_bytes, 340);
        assert_eq!(&bytes[4..8], &TRUEHD_MAJOR_SYNC);
        let aus: Vec<_> = AuIter::new(&bytes).collect::<Result<Vec<_>>>().unwrap();
        assert!(aus.len() > 200, "expected hundreds of AUs, got {}", aus.len());
        // 40-sample counter is continuous modulo 65536 for a clean live.bits read.
        let counters: Vec<u16> = aus.iter().map(|(_, h)| h.sample_counter).collect();
        for w in counters.windows(2) {
            assert_eq!(w[1], w[0].wrapping_add(40), "sample counter discontinuity");
        }
    }
}
