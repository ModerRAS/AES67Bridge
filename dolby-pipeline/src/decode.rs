//! TrueHD / DD+ decoding into bed + object PCM at 480-frame block granularity.
//!
//! TrueHD path: vendored \`truehdd\` (Apache-2.0) Extractor -> Parser -> Decoder. One access unit
//! carries 40 samples at 48 kHz. Presentation 2 (7.1 bed) and presentation 3 (object channels +
//! OAMD) are decoded in one pass; OAMD is only attached to presentation 3.
//!
//! Bed/object split follows NSP's bridge: OAMD object indices are presentation-3 channel indices;
//! \`b_object_in_bed_or_isf\` channels are bed members anchored by the OAMD bed assignment, the
//! rest are dynamic objects. If presentation 3 carries no OAMD (plain TrueHD), presentation 2 is
//! used as an 8-channel bed and there are no objects.
//!
//! E-AC-3 / AC-3 PCM decoding is the daemon/rustd side (per product scope). The vendored \`eac3\`
//! crate is linked for frame extraction/verification, see [\`eac3_scan\`].
//!
//! 40-sample AUs are accumulated into 480-sample (10 ms) blocks by [\`AuAssembler\`].

use crate::bits::TrueHdAuHeader;
use crate::layout::ChannelId;
use crate::n03::BED_LAYOUT_714;
use crate::oamd::{self, ObjectMetadata};
use crate::{PipelineError, Result};
use truehd::process::decode::{DecodedAccessUnit, Decoder};
use truehd::process::extract::Extractor;
use truehd::process::parse::Parser;
use truehd::process::{PresentationMap, PresentationType};
use truehd::structs::channel::ChannelLabel;

pub const SAMPLE_RATE: u32 = 48_000;
/// TrueHD access-unit samples at 48 kHz: \`(freq/44100)*40\`.
pub const AU_SAMPLES: usize = 40;
/// One 10 ms render/AES67 block.
pub const BLOCK_FRAMES: usize = 480;
/// i32 PCM full scale used by the vendored decoder (24-bit left aligned).
pub const I32_PCM_FULL_SCALE: f32 = (1u32 << 23) as f32;
/// Hard cap: ROAR's patched OLR batch element accepts 1..=128 objects.
pub const MAX_OBJECTS: usize = 128;

/// What the decoder is asked to produce.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DecodeMode {
    /// Presentation 3 object channels + OAMD (bed = presentation-3 bed members), falls back to
    /// presentation 2 when the stream has no OAMD. This is the Atmos path used by ROAR.
    #[default]
    Objects,
    /// Presentation 2 only (7.1 bed). Used for the no-object fallback and for bit-exact bed
    /// verification against ffmpeg.
    BedOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DecodeStats {
    pub es_bytes: u64,
    pub frames_extracted: u64,
    pub major_syncs: u64,
    pub aus_decoded: u64,
    pub aus_skipped_pre_sync: u64,
    pub extract_errors: u64,
    pub parse_errors: u64,
    pub decode_errors: u64,
    pub oamd_payloads: u64,
    pub discarded_samples: u64,
}

/// One decoded access unit (40 samples at 48 kHz), still unpacked.
#[derive(Clone, Debug, Default)]
pub struct DecodedAu {
    pub sample_length: usize,
    /// 12-channel 7.1.4 bed in \`BED_LAYOUT_714\` order; missing channels are silence.
    pub bed: Vec<Vec<f32>>,
    /// Dynamic object mono channels in OAMD id order.
    pub objects: Vec<Vec<f32>>,
    pub metadata: Option<ObjectMetadata>,
    pub p2_channels: usize,
    pub p3_channels: usize,
    pub major_sync: bool,
}

/// A completed 480-frame (10 ms) block.
#[derive(Clone, Debug, Default)]
pub struct DecodedBlock {
    pub frames: usize,
    /// Absolute sample position of the block start (48 kHz samples).
    pub sample_pos: u64,
    pub bed: Vec<Vec<f32>>,
    pub objects: Vec<Vec<f32>>,
    pub metadata: Option<ObjectMetadata>,
    pub object_count: usize,
    pub p2_channels: usize,
    pub p3_channels: usize,
    pub object_mode: bool,
}

fn label_to_channel(label: &ChannelLabel) -> Option<ChannelId> {
    Some(match label {
        ChannelLabel::L => ChannelId::FL,
        ChannelLabel::R => ChannelId::FR,
        ChannelLabel::C => ChannelId::FC,
        ChannelLabel::LFE => ChannelId::LFE,
        ChannelLabel::Ls => ChannelId::LS,
        ChannelLabel::Rs => ChannelId::RS,
        ChannelLabel::Lb => ChannelId::LRS,
        ChannelLabel::Rb => ChannelId::RRS,
        ChannelLabel::Tfl => ChannelId::TFL,
        ChannelLabel::Tfr => ChannelId::TFR,
        ChannelLabel::Tbl => ChannelId::TBL,
        ChannelLabel::Tbr => ChannelId::TBR,
        ChannelLabel::Tsl => ChannelId::TSL,
        ChannelLabel::Tsr => ChannelId::TSR,
        ChannelLabel::Lw => ChannelId::LW,
        ChannelLabel::Rw => ChannelId::RW,
        ChannelLabel::LFE2 => ChannelId::LFE2,
        ChannelLabel::Cb => ChannelId::CB,
        ChannelLabel::Tc => ChannelId::TC,
        _ => return None,
    })
}

fn bed_slot(channel: ChannelId) -> Option<usize> {
    BED_LAYOUT_714.iter().position(|&c| c == channel)
}

fn pcm_f32(au: &DecodedAccessUnit, channel: usize, sample: usize) -> f32 {
    au.pcm_data[sample][channel] as f32 / I32_PCM_FULL_SCALE
}

/// Decoded presentation-2 (or fallback) bed into the 12-channel 7.1.4 bed.
fn bed_from_channel_presentation(au: &DecodedAccessUnit) -> Vec<Vec<f32>> {
    let mut bed = vec![vec![0.0f32; au.sample_length]; 12];
    let count = au.channel_count.min(au.channel_labels.len().max(au.channel_count));
    let positional: Option<&[ChannelId]> = match au.channel_count {
        6 => Some(&[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS]),
        8 => Some(&[
            ChannelId::FL,
            ChannelId::FR,
            ChannelId::FC,
            ChannelId::LFE,
            ChannelId::LS,
            ChannelId::RS,
            ChannelId::LRS,
            ChannelId::RRS,
        ]),
        _ => None,
    };
    for channel in 0..count {
        let id = au
            .channel_labels
            .get(channel)
            .and_then(label_to_channel)
            .or_else(|| positional.and_then(|p| p.get(channel).copied()));
        let Some(id) = id else { continue };
        let Some(slot) = bed_slot(id) else { continue };
        for sample in 0..au.sample_length {
            bed[slot][sample] = pcm_f32(au, channel, sample);
        }
    }
    bed
}

impl TrueHdDecoder {
    fn decode_selected(&mut self, au: &truehd::structs::access_unit::AccessUnit) -> Result<Box<[Option<DecodedAccessUnit>; 4]>> {
        self.decoder
            .decode_presentations(au, &self.required)
            .map_err(|e| PipelineError::Decode(format!("decode_presentations: {e}")))
    }

    /// Recompute the presentation mask from a major sync's PresentationMap.
    ///
    /// Non-Atmos TrueHD streams may only carry presentation 1 (presentation 2 then resolves to a
    /// copy of it); Atmos streams carry presentation 3. Hard-requiring presentation 3 makes the
    /// decoder reject plain 5.1/7.1 TrueHD, which is what the device produced (dump2.bits).
    fn rearm_presentations(&mut self, frame: &[u8]) {
        if frame.len() < 22 {
            return;
        }
        let sync = u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]);
        let extended_substream_info = frame[20] & 0x0F;
        let substream_info = frame[21];
        let map = PresentationMap::for_format_sync(sync, substream_info, extended_substream_info);
        let highest = (0..4)
            .rev()
            .find(|&i| map.presentation_type_by_index(i) != PresentationType::Invalid);
        let mut required = [false; 4];
        match highest {
            Some(3) => {
                required[2] = true;
                required[3] = true;
            }
            Some(2) => required[2] = true,
            Some(1) => required[1] = true,
            Some(_) => required[0] = true,
            None => required[1] = true,
        }
        if self.mode == DecodeMode::BedOnly
            && map.presentation_type_by_index(2) != PresentationType::Invalid
        {
            required = [false, false, true, false];
        }
        if required != self.required {
            log::info!(
                "TrueHD presentations required {:?} (major-sync masks {:?})",
                required,
                map.masks
            );
            self.required = required;
            if self.stats.aus_decoded > 0 {
                // Mid-stream configuration change (player switched title/track, or a capture splice
                // crossed codecs): re-lock parser and decoder on this major sync. Without resetting
                // the parser too, every following AU fails to parse and the stream stalls.
                self.parser.reset_for_next_major_sync();
                self.decoder.reset_for_next_major_sync();
                self.cached_metadata = None;
            }
            self.parser.set_required_presentations(&self.required);
        }
    }
}

/// Streaming TrueHD decoder.
pub struct TrueHdDecoder {
    extractor: Extractor,
    parser: Parser,
    decoder: Decoder,
    stats: DecodeStats,
    /// Last OAMD attributes. OAMD is only carried every N access units (~185 ms); object PCM is in
    /// presentation 3 on every AU, so attributes must be carried forward until the next update.
    cached_metadata: Option<ObjectMetadata>,
    mode: DecodeMode,
    /// Presentations this stream needs, chosen from the major sync's PresentationMap.
    required: [bool; 4],
    /// Set after a decode error: skip access units until the next major sync and re-lock.
    awaiting_major_sync: bool,
}

impl Default for TrueHdDecoder {
    fn default() -> Self {
        Self::new()
    }
}

struct AuParts {
    bed: Vec<Vec<f32>>,
    objects: Vec<Vec<f32>>,
    metadata: Option<ObjectMetadata>,
    sample_length: usize,
    p2_channels: usize,
    p3_channels: usize,
}

impl TrueHdDecoder {
    pub fn new() -> Self {
        Self {
            extractor: Extractor::default(),
            parser: Parser::default(),
            decoder: Decoder::default(),
            stats: DecodeStats::default(),
            cached_metadata: None,
            mode: DecodeMode::Objects,
            required: [false, false, true, true],
            awaiting_major_sync: false,
        }
    }

    /// Bed-only decoder (presentation 2): no objects, no OAMD. Bit-exact with ffmpeg's 7.1 bed.
    pub fn bed_only() -> Self {
        Self { mode: DecodeMode::BedOnly, required: [false, false, true, false], ..Self::new() }
    }

    pub fn mode(&self) -> DecodeMode {
        self.mode
    }

    pub fn stats(&self) -> DecodeStats {
        self.stats
    }

    /// Feed elementary-stream bytes (live.bits bytes, unchanged) and return every complete
    /// access unit decoded by this call.
    pub fn push_es(&mut self, es: &[u8]) -> Vec<DecodedAu> {
        self.stats.es_bytes += es.len() as u64;
        self.extractor.push_bytes(es);
        let mut out = Vec::new();
        while let Some(item) = self.extractor.next() {
            let frame = match item {
                Ok(frame) => frame,
                Err(_error) => {
                    self.stats.extract_errors += 1;
                    continue;
                }
            };
            self.stats.frames_extracted += 1;
            let major_sync = frame.is_major_sync();
            if major_sync {
                self.stats.major_syncs += 1;
                self.rearm_presentations(&frame.data);
            }
            if self.awaiting_major_sync {
                if !major_sync {
                    self.stats.aus_skipped_pre_sync += 1;
                    continue;
                }
                self.awaiting_major_sync = false;
            }
            if !major_sync && self.stats.aus_decoded == 0 {
                // Decoder state needs a major sync before the first presentation map exists.
                self.stats.aus_skipped_pre_sync += 1;
                continue;
            }
            let au = match self.parser.parse(&frame) {
                Ok(au) => au,
                Err(_error) => {
                    self.stats.parse_errors += 1;
                    continue;
                }
            };
            match self.decode_selected(&au) {
                Ok(mut results) => {
                    let oamd_payloads =
                        results[3].as_ref().map(|p| p.oamd.len() as u64).unwrap_or(0);
                    match self.parts_from_results(&mut results) {
                        Ok(parts) => {
                            self.stats.aus_decoded += 1;
                            self.stats.oamd_payloads += oamd_payloads;
                            out.push(DecodedAu {
                                sample_length: parts.sample_length,
                                bed: parts.bed,
                                objects: parts.objects,
                                metadata: parts.metadata,
                                p2_channels: parts.p2_channels,
                                p3_channels: parts.p3_channels,
                                major_sync,
                            });
                        }
                        Err(error) => {
                            self.stats.decode_errors += 1;
                            log::warn!("dropping AU: {error}");
                            self.awaiting_major_sync = true;
                        }
                    }
                }
                Err(error) => {
                    self.stats.decode_errors += 1;
                    log::warn!("decode error, resync at next major sync: {error:#}");
                    self.reset_to_next_major_sync();
                }
            }
        }
        out
    }

    fn reset_to_next_major_sync(&mut self) {
        // Never replace the extractor: it holds buffered stream bytes. Replacing it drops the rest
        // of a multi-hundred-KB push after a single bad access unit (the dump2.bits bug).
        self.parser.reset_for_next_major_sync();
        self.decoder.reset_for_next_major_sync();
        self.cached_metadata = None;
        self.awaiting_major_sync = true;
    }

    fn parts_from_results(
        &mut self,
        results: &mut [Option<DecodedAccessUnit>; 4],
    ) -> Result<AuParts> {
        // OAMD only rides presentation 3, and only on some AUs; carry the last attributes forward
        // so the bed/object split does not flap between access units.
        if let Some(p3) = results[3].as_ref().filter(|p| !p.oamd.is_empty()) {
            let payload = p3.oamd.last().expect("checked non-empty");
            self.cached_metadata = Some(oamd::from_oamd(payload)?);
        }
        if self.mode == DecodeMode::Objects {
            if let (Some(p3), Some(meta)) = (results[3].as_ref(), self.cached_metadata.clone()) {
                if meta.dynamic_objects > MAX_OBJECTS {
                    return Err(PipelineError::Decode(format!(
                        "{} dynamic objects exceed the {MAX_OBJECTS}-object ceiling",
                        meta.dynamic_objects
                    )));
                }
                let mut bed = vec![vec![0.0f32; p3.sample_length]; 12];
                for (channel_index, channel) in meta.bed_channels() {
                    let Some(slot) = bed_slot(channel) else { continue };
                    for sample in 0..p3.sample_length {
                        bed[slot][sample] = pcm_f32(p3, channel_index, sample);
                    }
                }
                let mut objects = Vec::with_capacity(meta.dynamic_objects);
                for object in meta.dynamic() {
                    if object.index >= p3.channel_count {
                        continue;
                    }
                    let mut mono = Vec::with_capacity(p3.sample_length);
                    for sample in 0..p3.sample_length {
                        mono.push(pcm_f32(p3, object.index, sample));
                    }
                    objects.push(mono);
                }
                return Ok(AuParts {
                    bed,
                    objects,
                    metadata: Some(meta),
                    sample_length: p3.sample_length,
                    p2_channels: results[2].as_ref().map(|p| p.channel_count).unwrap_or(0),
                    p3_channels: p3.channel_count,
                });
            }
        }
        // Channel-based fallback: the highest-channel presentation the stream actually carries.
        // * Atmos before its first OAMD payload: presentation 2 (7.1).
        // * plain 5.1/7.1 TrueHD: presentation 2 resolves to a copy of presentation 1, so
        //   results[2] is None and the 6/8-channel presentation arrives in results[1].
        let bed_source = [3usize, 2, 1, 0]
            .iter()
            .filter_map(|&index| results[index].as_ref())
            .max_by_key(|au| au.channel_count)
            .ok_or_else(|| PipelineError::Decode("no decodable presentation".into()))?;
        Ok(AuParts {
            bed: bed_from_channel_presentation(bed_source),
            objects: Vec::new(),
            metadata: None,
            sample_length: bed_source.sample_length,
            p2_channels: bed_source.channel_count,
            p3_channels: results[3].as_ref().map(|p| p.channel_count).unwrap_or(0),
        })
    }
}

/// Accumulate variable access units into fixed 480-frame blocks.
pub struct AuAssembler {
    frames: usize,
    bed: Vec<Vec<f32>>,
    objects: Vec<Vec<f32>>,
    filled: usize,
    sample_pos: u64,
    metadata: Option<ObjectMetadata>,
    p2_channels: usize,
    p3_channels: usize,
    object_mode: bool,
    discarded_samples: u64,
}

impl AuAssembler {
    pub fn new(frames: usize) -> Self {
        Self {
            frames,
            bed: vec![vec![0.0; frames]; 12],
            objects: Vec::new(),
            filled: 0,
            sample_pos: 0,
            metadata: None,
            p2_channels: 0,
            p3_channels: 0,
            object_mode: false,
            discarded_samples: 0,
        }
    }

    pub fn discard_count(&self) -> u64 {
        self.discarded_samples
    }

    fn ensure_objects(&mut self, count: usize) {
        if self.objects.len() != count {
            let mut new_objects = vec![vec![0.0f32; self.frames]; count];
            let keep = self.filled.min(self.frames);
            for (dst, src) in new_objects.iter_mut().zip(self.objects.iter()) {
                dst[..keep].copy_from_slice(&src[..keep]);
            }
            self.objects = new_objects;
        }
    }

    pub fn push(&mut self, au: DecodedAu) -> Option<DecodedBlock> {
        if au.bed.len() != 12 {
            return None;
        }
        self.ensure_objects(au.objects.len());
        let remaining = self.frames.saturating_sub(self.filled);
        let take = au.sample_length.min(remaining);
        for (channel, samples) in au.bed.iter().enumerate() {
            self.bed[channel][self.filled..self.filled + take].copy_from_slice(&samples[..take]);
        }
        for (channel, samples) in au.objects.iter().enumerate() {
            self.objects[channel][self.filled..self.filled + take].copy_from_slice(&samples[..take]);
        }
        if take < au.sample_length {
            self.discarded_samples += (au.sample_length - take) as u64;
        }
        if au.metadata.is_some() {
            self.metadata = au.metadata;
        }
        self.p2_channels = au.p2_channels;
        self.p3_channels = au.p3_channels;
        self.object_mode = !self.objects.is_empty();
        self.filled += take;
        if self.filled < self.frames {
            return None;
        }
        let block = DecodedBlock {
            frames: self.frames,
            sample_pos: self.sample_pos,
            bed: std::mem::replace(&mut self.bed, vec![vec![0.0; self.frames]; 12]),
            objects: std::mem::take(&mut self.objects),
            metadata: self.metadata.take(),
            object_count: self.objects.len(),
            p2_channels: self.p2_channels,
            p3_channels: self.p3_channels,
            object_mode: self.object_mode,
        };
        // object_count was computed after take: recompute from the block.
        let block = DecodedBlock { object_count: block.objects.len(), ..block };
        self.sample_pos += self.frames as u64;
        self.filled = 0;
        Some(block)
    }
}

/// E-AC-3 / AC-3 frame extraction statistics (decoding PCM is the daemon side's job).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Eac3Scan {
    pub frames: usize,
    pub bytes: usize,
}

/// Walk E-AC-3/AC-3 syncframes with the vendored \`eac3\` crate (used by offline probes and to
/// verify live.bits classification).
pub fn eac3_scan(es: &[u8]) -> Result<Eac3Scan> {
    let mut extractor = eac3::Extractor::default();
    extractor.push_bytes(es);
    let mut stats = Eac3Scan::default();
    loop {
        match extractor.next_frame() {
            Ok(Some(frame)) => {
                stats.frames += 1;
                stats.bytes += frame.as_bytes().len();
            }
            Ok(None) => break,
            Err(e) => return Err(PipelineError::Decode(format!("eac3 extract: {e:?}"))),
        }
    }
    Ok(stats)
}

/// TrueHD AU header helper (diagnostics): the first AU of the stream.
pub fn first_au_header(es: &[u8]) -> Result<TrueHdAuHeader> {
    TrueHdAuHeader::parse(es)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic 480-frame block: 12 bed channels of constant value, N objects of constant value.
    fn synthetic_block(objects: usize) -> DecodedBlock {
        let frames = BLOCK_FRAMES;
        DecodedBlock {
            frames,
            sample_pos: 0,
            bed: (0..12).map(|c| vec![c as f32 + 1.0; frames]).collect(),
            objects: (0..objects).map(|o| vec![100.0 + o as f32; frames]).collect(),
            metadata: None,
            object_count: objects,
            p2_channels: 8,
            p3_channels: 12,
            object_mode: objects > 0,
        }
    }

    #[test]
    fn assembler_emits_exactly_480_frames_from_40_sample_aus() {
        let mut assembler = AuAssembler::new(BLOCK_FRAMES);
        let mut blocks = Vec::new();
        for au_index in 0..24 {
            let au = DecodedAu {
                sample_length: AU_SAMPLES,
                bed: (0..12).map(|c| vec![au_index as f32 + c as f32; AU_SAMPLES]).collect(),
                objects: vec![vec![1.0f32; AU_SAMPLES]],
                metadata: None,
                p2_channels: 8,
                p3_channels: 12,
                major_sync: au_index == 0,
            };
            if let Some(block) = assembler.push(au) {
                blocks.push(block);
            }
        }
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].frames, 480);
        assert_eq!(blocks[0].sample_pos, 0);
        assert_eq!(blocks[1].sample_pos, 480);
        assert_eq!(blocks[0].object_count, 1);
        assert_eq!(assembler.discard_count(), 0);
    }

    #[test]
    fn assembler_handles_object_count_change_between_blocks() {
        let mut assembler = AuAssembler::new(BLOCK_FRAMES);
        let mut blocks = 0;
        for au_index in 0..24 {
            let objects = if au_index < 12 { 1 } else { 3 };
            let au = DecodedAu {
                sample_length: AU_SAMPLES,
                bed: vec![vec![0.0; AU_SAMPLES]; 12],
                objects: (0..objects).map(|o| vec![o as f32; AU_SAMPLES]).collect(),
                metadata: None,
                p2_channels: 8,
                p3_channels: 12,
                major_sync: au_index == 0,
            };
            if assembler.push(au).is_some() {
                blocks += 1;
            }
        }
        assert_eq!(blocks, 2);
    }

    #[test]
    fn assembler_discards_non_multiple_tail() {
        let mut assembler = AuAssembler::new(BLOCK_FRAMES);
        let au = DecodedAu {
            sample_length: 100,
            bed: vec![vec![0.0; 100]; 12],
            objects: Vec::new(),
            metadata: None,
            p2_channels: 8,
            p3_channels: 0,
            major_sync: true,
        };
        assert!(assembler.push(au).is_none());
        // 5 x 100 = 500 -> first block 480, 20 samples of the 5th AU discarded.
        let mut blocks = 0;
        for _ in 0..4 {
            let au = DecodedAu {
                sample_length: 100,
                bed: vec![vec![0.0; 100]; 12],
                objects: Vec::new(),
                metadata: None,
                p2_channels: 8,
                p3_channels: 0,
                major_sync: false,
            };
            if assembler.push(au).is_some() {
                blocks += 1;
            }
        }
        assert_eq!(blocks, 1);
        assert!(assembler.discard_count() > 0);
    }

    #[test]
    fn layout_bed_slots_are_714() {
        assert_eq!(bed_slot(ChannelId::FL), Some(0));
        assert_eq!(bed_slot(ChannelId::LRS), Some(6));
        assert_eq!(bed_slot(ChannelId::TBR), Some(11));
        assert_eq!(bed_slot(ChannelId::LW), None);
    }

    #[test]
    fn synthetic_block_is_valid() {
        let block = synthetic_block(3);
        assert_eq!(block.bed.len(), 12);
        assert_eq!(block.objects.len(), 3);
        assert_eq!(block.bed[0].len(), BLOCK_FRAMES);
    }
}
