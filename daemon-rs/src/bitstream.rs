//! live.bits decoder: raw AC-3 / E-AC-3 elementary stream -> core PCM.
//!
//! The HAL tee (v5+) exports the compressed bytes the vendor audio HAL passes
//! to the HDMI/SPDIF output into /data/haltee/live.bits, with a companion
//! live.bits.meta describing the vendor format (0x09 AC3, 0x0A E_AC3, ...).
//! This module only decodes the core channel PCM; object/TrueHD rendering
//! belongs to the roarport pipeline. It intentionally contains no DSP: the
//! decoded PCM goes straight into the AES67 ring.

use eac3::{CorePcmFrame, Extractor, PcmDecoder};

use crate::config::{layout_for_label_set, layout_for_labels, layout_labels};

/// One decoded access unit, already in AES67 canonical channel order.
#[derive(Clone, Debug)]
pub struct DecodedFrame {
    pub sample_rate: u32,
    pub channels: usize,
    /// Actual source layout name (for example 5.1) when the label set matches a
    /// known layout; None for unknown channel shapes.
    pub layout: Option<String>,
    pub labels: Vec<String>,
    /// Interleaved normalized samples (-1.0 ..= 1.0), channels per frame.
    pub samples: Vec<f32>,
}

/// Incremental raw ES decoder. push_bytes() accepts arbitrary chunks; the
/// extractor finds 0x0B77 syncframes and the decoder keeps cross-frame state.
#[derive(Debug)]
pub struct BitsDecoder {
    extractor: Extractor,
    decoder: PcmDecoder,
    pub frames: u64,
    pub errors: u64,
}

impl Default for BitsDecoder {
    fn default() -> Self {
        Self {
            extractor: Extractor::default(),
            decoder: PcmDecoder::new(),
            frames: 0,
            errors: 0,
        }
    }
}

impl BitsDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.extractor.push_bytes(bytes);
    }

    pub fn pending_bytes(&self) -> usize {
        self.extractor.buffered_len()
    }

    /// Decode the next complete access unit. Returns None until a whole frame is
    /// buffered; malformed frames are counted and skipped (sync is recovered by
    /// the extractor, decoder state is reset).
    pub fn next_frame(&mut self) -> Option<DecodedFrame> {
        loop {
            let frame = match self.extractor.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => return None,
                Err(_) => {
                    self.errors += 1;
                    self.decoder.reset();
                    continue;
                }
            };
            let info = frame.info();
            let decoded = if info.bitstream_id <= 10 {
                self.decoder.push_legacy_ac3_access_unit(frame.as_bytes())
            } else {
                self.decoder.push_access_unit(frame.as_bytes())
            };
            match decoded {
                Ok(result) => {
                    self.frames += 1;
                    match convert(result.pcm) {
                        Some(decoded) => return Some(decoded),
                        None => {
                            self.errors += 1;
                            continue;
                        }
                    }
                }
                Err(_) => {
                    self.errors += 1;
                    self.decoder.reset();
                    continue;
                }
            }
        }
    }
}

fn channel_label(channel: eac3::BedChannel) -> &'static str {
    use eac3::BedChannel::*;
    match channel {
        FrontLeft => "L",
        FrontRight => "R",
        Center => "C",
        LowFrequencyEffects => "LFE",
        SurroundLeft => "Ls",
        SurroundRight => "Rs",
        RearCenter => "Cb",
        RearLeft => "Lb",
        RearRight => "Rb",
        TopFrontLeft => "Tfl",
        TopFrontRight => "Tfr",
        TopSurroundLeft => "Tsl",
        TopSurroundRight => "Tsr",
        TopRearLeft => "Tbl",
        TopRearRight => "Tbr",
        WideLeft => "Lw",
        WideRight => "Rw",
        LowFrequencyEffects2 => "LFE2",
    }
}

fn convert(core: CorePcmFrame) -> Option<DecodedFrame> {
    let samples = core.samples_per_channel();
    if samples == 0 {
        return None;
    }
    let mut entries: Vec<(String, Vec<f32>)> = Vec::new();
    for (index, &channel) in core.fullband_channel_order.iter().enumerate() {
        let data = core.fullband_channels.get(index)?;
        if data.len() == samples {
            entries.push((channel_label(channel).to_string(), data.clone()));
        }
    }
    if let Some(lfe) = core.lfe_channel {
        if lfe.len() == samples {
            entries.push(("LFE".to_string(), lfe));
        }
    }
    if entries.is_empty() {
        return None;
    }
    let label_refs: Vec<&str> = entries.iter().map(|(label, _)| label.as_str()).collect();
    let layout = layout_for_labels(&label_refs)
        .or_else(|| layout_for_label_set(&label_refs.iter().map(|s| s.to_string()).collect::<Vec<_>>()))
        .map(str::to_string);
    // Known layouts get canonical AES67 channel order (5.1 = L,R,C,LFE,Ls,Rs);
    // unknown shapes keep the decoder order and their decoded labels.
    let order: Vec<usize> = match layout.as_deref() {
        Some(name) => {
            let canonical = layout_labels(name)?;
            let mut order = Vec::with_capacity(entries.len());
            for want in canonical {
                let index = entries
                    .iter()
                    .position(|(label, _)| label == want)
                    .unwrap_or(0);
                order.push(index);
            }
            order
        }
        None => (0..entries.len()).collect(),
    };
    let channels = entries.len();
    let mut interleaved = Vec::with_capacity(samples * channels);
    for frame in 0..samples {
        for &index in &order {
            interleaved.push(entries[index].1[frame]);
        }
    }
    let labels: Vec<String> = order.iter().map(|&index| entries[index].0.clone()).collect();
    Some(DecodedFrame {
        sample_rate: core.sample_rate,
        channels,
        layout,
        labels,
        samples: interleaved,
    })
}
