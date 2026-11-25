//! PCM (non-object) rendering through ROAR.
//!
//! The PCM transport path has no OAMD: it carries plain channels. Instead of writing a bridge-side
//! downmix matrix, the real source layout is declared as a **channel-based element** and ROAR maps it
//! onto the target layout:
//!
//! * source != target and the pair is a legal IAMF downmix ([IAMF downmix][1]) -> ROAR's
//!   `DownmixRenderer` applies the IAMF coefficients (only down, never up; no height -> no-height);
//! * otherwise (same layout, upmix, invalid pair) -> ROAR's EAR renderer pans the channels;
//! * LFE stays inside the element (it is never panned) and reaches the target LFE channel.
//!
//! [1]: `vendor/roar` `common/downmix.rs`.

use aom_roar::{
    AudioElementConfig, ChannelBasedConfig, Config, Decibels, DownmixInfo, DownmixMode, GroupId,
    InputLayout, LinearGain, OutputLayout, PlanarBufferMut, PlanarBufferRef, RoarRenderer,
    SampleRate, Samples,
};

use crate::layout::{ChannelId, LayoutSpec};
use crate::render::output_layout_for;
use crate::{PipelineError, Result};

const PCM_ELEMENT_ID: u32 = 20_000;

/// PCM renderer configuration.
#[derive(Clone, Debug)]
pub struct PcmConfig {
    /// Daemon layout id of the incoming PCM (e.g. "5.1", "7.1.2").
    pub source: String,
    /// Daemon layout id published to AES67 (e.g. "7.1.4").
    pub target: String,
    /// Block size in frames (480 = 10 ms at 48 kHz).
    pub frames: usize,
    /// IAMF demixing mode used when ROAR picks the downmix renderer (default mode 2: all 0.707).
    pub downmix_mode: DownmixMode,
    /// Enable ROAR's look-ahead peak limiter (default on, same policy as the object path).
    pub limiter: bool,
    /// Limiter ceiling in dBFS (default -1.0).
    pub limiter_threshold_db: f32,
}

impl PcmConfig {
    pub fn new(source: impl Into<String>, target: impl Into<String>, frames: usize) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            frames,
            downmix_mode: DownmixMode::Mode2PosOffset,
            limiter: true,
            limiter_threshold_db: -1.0,
        }
    }
}

fn input_layout_for(id: &str) -> Result<InputLayout> {
    Ok(match id {
        "2.0" => InputLayout::Stereo,
        "5.1" => InputLayout::Layout51,
        "5.1.2" => InputLayout::Layout512,
        "5.1.4" => InputLayout::Layout514,
        "7.1" => InputLayout::Layout71,
        "7.1.2" => InputLayout::Layout712,
        "7.1.4" => InputLayout::Layout714,
        "9.1.6" => InputLayout::Layout916,
        other => {
            return Err(PipelineError::Unsupported(format!(
                "source layout {other:?} has no ROAR input layout (validated patch has no 4.1.2)"
            )))
        }
    })
}

fn channel_index_order(from: &LayoutSpec, to: &LayoutSpec) -> Result<Vec<usize>> {
    to.channels()
        .iter()
        .map(|channel| {
            from.index_of(*channel).ok_or_else(|| {
                PipelineError::Config(format!(
                    "channel {channel:?} missing from layout {}",
                    from.id()
                ))
            })
        })
        .collect()
}

/// PCM channel renderer: one ROAR instance per AES67 pipeline.
pub struct PcmRenderer {
    frames: Samples,
    rdr: RoarRenderer,
    group: GroupId,
    source_layout: LayoutSpec,
    target_layout: LayoutSpec,
    roar_source: LayoutSpec,
    roar_target: LayoutSpec,
    in_to_roar: Vec<usize>,
    roar_to_out: Vec<usize>,
    in_channels: usize,
    out_channels: usize,
    in_scratch: Vec<Vec<f32>>,
    out_scratch: Vec<Vec<f32>>,
    limiter: bool,
    limiter_ceiling: f32,
    limited_samples: u64,
    out_peak: f32,
    rendered_blocks: u64,
}

impl PcmRenderer {
    pub fn new(config: &PcmConfig) -> Result<Self> {
        if config.frames == 0 || config.frames > 4096 {
            return Err(PipelineError::Config(format!(
                "invalid block frames {}",
                config.frames
            )));
        }
        let source_layout = LayoutSpec::known(&config.source).ok_or_else(|| {
            PipelineError::Unsupported(format!("unknown source layout {:?}", config.source))
        })?;
        let target_layout = LayoutSpec::known(&config.target).ok_or_else(|| {
            PipelineError::Unsupported(format!("unknown target layout {:?}", config.target))
        })?;
        let roar_source = LayoutSpec::roar(&config.source).ok_or_else(|| {
            PipelineError::Unsupported(format!("ROAR has no order for layout {:?}", config.source))
        })?;
        let roar_target = LayoutSpec::roar(&config.target).ok_or_else(|| {
            PipelineError::Unsupported(format!("ROAR has no order for layout {:?}", config.target))
        })?;
        if roar_source.len() != source_layout.len() || roar_target.len() != target_layout.len() {
            return Err(PipelineError::Config(format!(
                "layout order mismatch for {} -> {}",
                config.source, config.target
            )));
        }
        let input_layout = input_layout_for(&config.source)?;
        let output_layout = output_layout_for(&config.target)?;
        let samples = Samples::new(config.frames as u32)
            .map_err(|e| PipelineError::Config(format!("Samples: {e:?}")))?;
        let roar_config = Config::new(
            output_layout,
            samples,
            SampleRate::new(crate::decode::SAMPLE_RATE)
                .map_err(|e| PipelineError::Config(format!("SampleRate: {e:?}")))?,
        )
        .map_err(|e| PipelineError::Render(format!("Config: {e:?}")))?;
        let mut rdr = RoarRenderer::create(&roar_config)
            .map_err(|e| PipelineError::Render(format!("RoarRenderer::create: {e:?}")))?;
        let group = rdr
            .add_audio_group()
            .map_err(|e| PipelineError::Render(format!("add_audio_group: {e:?}")))?;
        rdr.add_element(
            group,
            PCM_ELEMENT_ID,
            &AudioElementConfig::ChannelBased(ChannelBasedConfig {
                layout: input_layout,
                downmix_info: Some(DownmixInfo::new(config.downmix_mode, None)),
                rendering_config: None,
            }),
        )
        .map_err(|e| PipelineError::Render(format!("add PCM element: {e:?}")))?;

        let limiter_ceiling = LinearGain::from(
            Decibels::new(config.limiter_threshold_db)
                .map_err(|e| PipelineError::Config(format!("limiter threshold: {e:?}")))?,
        )
        .value();
        if config.limiter {
            rdr.enable_limiter(true)
                .map_err(|e| PipelineError::Render(format!("enable limiter: {e:?}")))?;
            rdr.set_limiter_threshold(
                Decibels::new(config.limiter_threshold_db)
                    .map_err(|e| PipelineError::Config(format!("limiter threshold: {e:?}")))?,
            )
            .map_err(|e| PipelineError::Render(format!("set limiter threshold: {e:?}")))?;
        }

        let in_channels = source_layout.len();
        let out_channels = target_layout.len();
        let in_to_roar = channel_index_order(&source_layout, &roar_source)?;
        let roar_to_out = channel_index_order(&roar_target, &target_layout)?;
        Ok(Self {
            frames: samples,
            rdr,
            group,
            source_layout,
            target_layout,
            roar_source,
            roar_target,
            in_to_roar,
            roar_to_out,
            in_channels,
            out_channels,
            in_scratch: vec![vec![0.0; config.frames]; in_channels],
            out_scratch: vec![vec![0.0; config.frames]; out_channels],
            limiter: config.limiter,
            limiter_ceiling,
            limited_samples: 0,
            out_peak: 0.0,
            rendered_blocks: 0,
        })
    }

    pub fn source_layout(&self) -> &LayoutSpec {
        &self.source_layout
    }

    pub fn target_layout(&self) -> &LayoutSpec {
        &self.target_layout
    }

    pub fn limiter_enabled(&self) -> bool {
        self.limiter
    }

    pub fn limiter_delay(&self) -> usize {
        if self.limiter {
            self.rdr.limiter_delay()
        } else {
            0
        }
    }

    pub fn limited_samples(&self) -> u64 {
        self.limited_samples
    }

    pub fn out_peak(&self) -> f32 {
        self.out_peak
    }

    pub fn rendered_blocks(&self) -> u64 {
        self.rendered_blocks
    }

    /// Render one PCM block.
    ///
    /// `input` is planar f32 in **daemon (source layout) order**; the returned channels are planar
    /// f32 in **daemon target layout order**.
    pub fn render_block(&mut self, input: &[Vec<f32>]) -> Result<Vec<Vec<f32>>> {
        let frames = self.frames.value() as usize;
        if input.len() != self.in_channels {
            return Err(PipelineError::Render(format!(
                "input has {} channels, source layout {} has {}",
                input.len(),
                self.source_layout.id(),
                self.in_channels
            )));
        }
        for (index, channel) in input.iter().enumerate() {
            if channel.len() < frames {
                return Err(PipelineError::Render(format!(
                    "input channel {index} has {} samples, expected {frames}",
                    channel.len()
                )));
            }
        }
        for (dst, &src) in self.in_scratch.iter_mut().zip(self.in_to_roar.iter()) {
            dst.copy_from_slice(&input[src][..frames]);
        }
        for channel in self.out_scratch.iter_mut() {
            channel.fill(0.0);
        }
        {
            let in_refs: Vec<&[f32]> = self.in_scratch.iter().map(|c| c.as_slice()).collect();
            let in_buf = PlanarBufferRef::new(&in_refs, self.in_channels, self.frames)
                .map_err(|e| PipelineError::Render(format!("input buffer: {e:?}")))?;
            let mut out_refs: Vec<&mut [f32]> =
                self.out_scratch.iter_mut().map(|c| c.as_mut_slice()).collect();
            let mut out_buf = PlanarBufferMut::new(&mut out_refs, self.out_channels, self.frames)
                .map_err(|e| PipelineError::Render(format!("output buffer: {e:?}")))?;
            self.rdr
                .render(&[(PCM_ELEMENT_ID, in_buf)], &mut out_buf)
                .map_err(|e| PipelineError::Render(format!("render PCM: {e:?}")))?;
        }
        self.rendered_blocks += 1;
        if self.limiter {
            let ceiling = self.limiter_ceiling;
            let mut peak = 0.0f32;
            let mut limited = 0u64;
            for channel in &self.out_scratch {
                for &sample in channel.iter() {
                    let magnitude = sample.abs();
                    if magnitude > peak {
                        peak = magnitude;
                    }
                    if magnitude >= ceiling * (1.0 - 1.0e-6) {
                        limited += 1;
                    }
                }
            }
            if peak > self.out_peak {
                self.out_peak = peak;
            }
            self.limited_samples += limited;
        }
        let mut output = vec![vec![0.0f32; frames]; self.out_channels];
        for (out, &roar) in output.iter_mut().zip(self.roar_to_out.iter()) {
            out.copy_from_slice(&self.out_scratch[roar]);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAMES: usize = 480;

    fn tone(freq: f32, amplitude: f32) -> Vec<f32> {
        let w = 2.0 * std::f32::consts::PI * freq / crate::decode::SAMPLE_RATE as f32;
        (0..FRAMES).map(|i| amplitude * (w * i as f32).sin()).collect()
    }

    fn silent() -> Vec<f32> {
        vec![0.0; FRAMES]
    }

    fn channel(spec: &LayoutSpec, id: ChannelId) -> usize {
        spec.index_of(id).unwrap()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn renderer(source: &str, target: &str) -> PcmRenderer {
        PcmRenderer::new(&PcmConfig::new(source, target, FRAMES)).unwrap()
    }

    #[test]
    fn downmix_folds_back_and_top_into_available_speakers() {
        let mut r = renderer("7.1.4", "5.1.2");
        let source = r.source_layout().clone();
        let mut input = vec![silent(); source.len()];
        input[channel(&source, ChannelId::LRS)] = tone(700.0, 0.5);
        input[channel(&source, ChannelId::TBL)] = tone(900.0, 0.4);
        let out = r.render_block(&input).unwrap();
        let target = r.target_layout().clone();
        assert_eq!(out.len(), 8);
        assert!(out.iter().flatten().all(|s| s.is_finite()));
        // LFE is not panned and stays silent when the source LFE is silent.
        assert!(rms(&out[channel(&target, ChannelId::LFE)]) < 1.0e-4);
        // The back-left fold lands on the left surround (IAMF mode 2: 0.707 gamma/beta path).
        let ls = rms(&out[channel(&target, ChannelId::LS)]);
        assert!(ls > 0.02, "back-left fold missing from Ls (rms {ls})");
        // The top-back fold lands on the top-front speaker.
        let tfl = rms(&out[channel(&target, ChannelId::TFL)]);
        assert!(tfl > 0.02, "top-back fold missing from Tfl (rms {tfl})");
        // Nothing appears on the opposite side.
        assert!(rms(&out[channel(&target, ChannelId::RS)]) < ls * 0.2);
    }

    #[test]
    fn same_layout_passes_channels_and_lfe() {
        let mut r = renderer("5.1", "5.1");
        let source = r.source_layout().clone();
        let mut input = vec![silent(); source.len()];
        for (index, id) in [
            ChannelId::FL,
            ChannelId::FR,
            ChannelId::FC,
            ChannelId::LS,
            ChannelId::RS,
        ]
        .iter()
        .enumerate()
        {
            input[channel(&source, *id)] = tone(300.0 + 100.0 * index as f32, 0.25);
        }
        input[channel(&source, ChannelId::LFE)] = tone(60.0, 0.2);
        let out = r.render_block(&input).unwrap();
        let target = r.target_layout().clone();
        for id in [
            ChannelId::FL,
            ChannelId::FR,
            ChannelId::FC,
            ChannelId::LS,
            ChannelId::RS,
            ChannelId::LFE,
        ] {
            let before = rms(&input[channel(&source, id)]);
            let after = rms(&out[channel(&target, id)]);
            assert!(after > before * 0.5, "{id:?} lost in EAR identity: {before} -> {after}");
            assert!(after < before * 2.0, "{id:?} boosted in EAR identity: {before} -> {after}");
        }
        assert!(r.rendered_blocks() == 1);
    }

    #[test]
    fn limiter_caps_loud_pcm() {
        let mut r = renderer("7.1.4", "5.1.2");
        let source = r.source_layout().clone();
        let input = vec![vec![2.0f32; FRAMES]; source.len()];
        let out = r.render_block(&input).unwrap();
        let peak = out
            .iter()
            .flat_map(|channel| channel.iter())
            .fold(0.0f32, |max, sample| max.max(sample.abs()));
        let ceiling = 0.891_251; // -1 dBFS
        assert!(peak <= ceiling + 1.0e-4, "PCM limiter ceiling exceeded: {peak}");
        assert!(r.limited_samples() > 0);
        assert!(r.limiter_delay() > 0);
    }

    #[test]
    fn rejects_unsupported_layouts() {
        assert!(PcmRenderer::new(&PcmConfig::new("4.1.2", "7.1.4", FRAMES)).is_err());
        assert!(PcmRenderer::new(&PcmConfig::new("7.1.4", "4.1.2", FRAMES)).is_err());
        assert!(PcmRenderer::new(&PcmConfig::new("7.1.4", "7.1.4", 0)).is_err());
    }
}
