//! ROAR object rendering (7.1.4 / 5.1.2 / 9.1.6 target layouts).
//!
//! This is the only place objects become loudspeaker channels. ROAR is linked statically as a
//! Rust crate (aom-roar 1.1.0 + NSP \`roar-1.1-validated.patch\`, vendored under
//! \`vendor/roar\`): the validated patch adds \`ObjectBasedConfig::new_nsp_olr_batch(1..=128)\`,
//! \`prepare_olr_geometry\`, fixed-block OLR backends and the 7.1.6/9.1.4 layouts. The C ABI is not
//! used because upstream \`roar_object_based_config_t\` is capped at 2 objects.
//!
//! Block sequence (same as NSP Croksor's OLR Engine):
//! 1. update all dynamic object positions for the block (ROAR interpolation expires otherwise),
//! 2. submit one grouped object-batch element + one 12ch 7.1.4 bed element,
//! 3. render to planar f32 in ROAR speaker order (caller maps it to the daemon layout order).
//!
//! No bridge-side DSP: this module does not mix down/up, EQ or delay; it only spatialises objects
//! and maps the 7.1.4 bed onto the target layout (ROAR's bed renderer). The only signal processing
//! is ROAR's own look-ahead peak limiter on the final output, enabled by default (-1 dBFS ceiling,
//! 5 ms look-ahead) so rendered peaks above full scale cannot clip the L24 conversion; see
//! [`RenderConfig::limiter`] and [`RenderConfig::limiter_threshold_db`].

use aom_roar::{
    AudioElementConfig, CartesianCoordinate, ChannelBasedConfig, Config, Decibels, GroupId,
    InputLayout, LinearGain, ObjectBasedConfig, ObjectPosition, OutputLayout, PlanarBufferMut,
    PlanarBufferRef, RoarRenderer, SampleRate, Samples,
};
use crate::decode::DecodedBlock;
use crate::layout::{LayoutSpec};
use crate::oamd::gain_db_to_linear;
use crate::{PipelineError, Result};

const BED_ID: u32 = 10_000; // NSP BED_ID
const OBJ_ID: u32 = 1; // NSP grouped object element id
const BED_CHANNELS: usize = 12;

/// Renderer configuration.
#[derive(Clone, Debug)]
pub struct RenderConfig {
    /// Declared target layout id (daemon layout vocabulary).
    pub target: String,
    /// Block size in frames (480 = 10 ms at 48 kHz).
    pub frames: usize,
    /// Hard cap for dynamic objects; values above 128 are rejected.
    pub max_objects: usize,
    /// Enable ROAR's look-ahead peak limiter on the final output (default on).
    pub limiter: bool,
    /// Limiter ceiling in dBFS (default -1.0, ROAR's own default).
    pub limiter_threshold_db: f32,
}

impl RenderConfig {
    pub fn new(target: impl Into<String>, frames: usize) -> Self {
        Self {
            target: target.into(),
            frames,
            max_objects: crate::decode::MAX_OBJECTS,
            limiter: true,
            limiter_threshold_db: -1.0,
        }
    }

    /// Renderer config with the limiter off (bit-transparent output; used by tests that
    /// assert pre-limiter levels).
    pub fn without_limiter(target: impl Into<String>, frames: usize) -> Self {
        let mut config = Self::new(target, frames);
        config.limiter = false;
        config
    }
}

/// ROAR output layout selector for a daemon layout id.
pub fn output_layout_for(id: &str) -> Result<OutputLayout> {
    Ok(match id {
        "2.0" => OutputLayout::Stereo,
        "5.1" => OutputLayout::Layout51,
        "5.1.2" => OutputLayout::Layout512,
        "5.1.4" => OutputLayout::Layout514,
        "7.1" => OutputLayout::Layout71,
        "7.1.2" => OutputLayout::Layout712,
        "7.1.4" => OutputLayout::Layout714,
        "7.1.6" => OutputLayout::Layout716,
        "9.1.4" => OutputLayout::Layout914,
        "9.1.6" => OutputLayout::Layout916,
        other => {
            return Err(PipelineError::Unsupported(format!(
                "target layout {other:?} has no ROAR output layout (validated patch has no 4.1.2)"
            )));
        }
    })
}

/// ROAR speaker order for a daemon layout id (canonical channel ids, ROAR order).
pub fn roar_layout_for(id: &str) -> Result<LayoutSpec> {
    LayoutSpec::roar(id)
        .ok_or_else(|| PipelineError::Unsupported(format!("ROAR layout {id:?} is not mapped")))
}

/// Object renderer: one ROAR instance per AES67 pipeline (multi-stream is a daemon-side split of
/// the planar output; never create one renderer per stream).
pub struct ObjectRenderer {
    frames: Samples,
    rdr: RoarRenderer,
    group: GroupId,
    out_channels: usize,
    roar_layout: LayoutSpec,
    target_layout: LayoutSpec,
    object_count: usize,
    object_element_active: bool,
    positions: Vec<CartesianCoordinate>,
    out_scratch: Vec<Vec<f32>>,
    bed_scratch: Vec<Vec<f32>>,
    obj_scratch: Vec<Vec<f32>>,
    max_objects: usize,
    rendered_blocks: u64,
    limiter: bool,
    limiter_ceiling: f32,
    limited_samples: u64,
    out_peak: f32,
}

impl ObjectRenderer {
    pub fn new(config: &RenderConfig) -> Result<Self> {
        if config.frames == 0 || config.frames > 4096 {
            return Err(PipelineError::Config(format!("invalid block frames {}", config.frames)));
        }
        if config.max_objects == 0 || config.max_objects > crate::decode::MAX_OBJECTS {
            return Err(PipelineError::Config(format!(
                "max_objects {} outside 1..={}",
                config.max_objects,
                crate::decode::MAX_OBJECTS
            )));
        }
        let target_layout = LayoutSpec::known(&config.target).ok_or_else(|| {
            PipelineError::Unsupported(format!("unknown target layout {:?}", config.target))
        })?;
        let output_layout = output_layout_for(&config.target)?;
        let roar_layout = roar_layout_for(&config.target)?;
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
        rdr.set_object_position_metadata_unit_to_process(samples)
            .map_err(|e| PipelineError::Render(format!("metadata unit: {e:?}")))?;
        rdr.prepare_olr_geometry()
            .map_err(|e| PipelineError::Render(format!("prepare_olr_geometry: {e:?}")))?;
        let group = rdr
            .add_audio_group()
            .map_err(|e| PipelineError::Render(format!("add_audio_group: {e:?}")))?;
        // The bed is always a 12-channel 7.1.4 channel-based element (NSP does the same; ROAR's
        // bed/ear renderer maps it onto the target layout). Missing bed channels are silence.
        rdr.add_element(
            group,
            BED_ID,
            &AudioElementConfig::ChannelBased(ChannelBasedConfig {
                layout: InputLayout::Layout714,
                downmix_info: None,
                rendering_config: None,
            }),
        )
        .map_err(|e| PipelineError::Render(format!("add bed element: {e:?}")))?;

        let out_channels = target_layout.len();
        if roar_layout.len() != out_channels {
            return Err(PipelineError::Render(format!(
                "ROAR layout {} has {} channels, target {} has {}",
                roar_layout.id(),
                roar_layout.len(),
                target_layout.id(),
                out_channels
            )));
        }
        let limiter_ceiling = LinearGain::from(
            Decibels::new(config.limiter_threshold_db)
                .map_err(|e| PipelineError::Config(format!("limiter threshold: {e:?}")))?,
        )
        .value();
        if config.limiter {
            rdr.enable_limiter(true)
                .map_err(|e| PipelineError::Render(format!("enable limiter: {e:?}")))?;
            rdr.set_limiter_threshold(Decibels::new(config.limiter_threshold_db).map_err(|e| {
                PipelineError::Config(format!("limiter threshold: {e:?}"))
            })?)
            .map_err(|e| PipelineError::Render(format!("set limiter threshold: {e:?}")))?;
        }
        Ok(Self {
            frames: samples,
            rdr,
            group,
            out_channels,
            roar_layout,
            target_layout,
            object_count: 0,
            object_element_active: false,
            positions: Vec::new(),
            out_scratch: vec![vec![0.0; config.frames]; out_channels],
            bed_scratch: vec![vec![0.0; config.frames]; BED_CHANNELS],
            obj_scratch: Vec::new(),
            max_objects: config.max_objects,
            rendered_blocks: 0,
            limiter: config.limiter,
            limiter_ceiling,
            limited_samples: 0,
            out_peak: 0.0,
        })
    }

    pub fn target_layout(&self) -> &LayoutSpec {
        &self.target_layout
    }

    /// Layout of the planar buffers returned by [\`render_block\`] (ROAR speaker order).
    pub fn roar_layout(&self) -> &LayoutSpec {
        &self.roar_layout
    }

    pub fn object_count(&self) -> usize {
        self.object_count
    }

    pub fn rendered_blocks(&self) -> u64 {
        self.rendered_blocks
    }

    /// Whether the ROAR look-ahead peak limiter is enabled on the output.
    pub fn limiter_enabled(&self) -> bool {
        self.limiter
    }

    /// Limiter look-ahead delay in samples per channel (0 when disabled).
    pub fn limiter_delay(&self) -> usize {
        self.limiter_delay_samples()
    }

    fn limiter_delay_samples(&self) -> usize {
        if self.limiter { self.rdr.limiter_delay() } else { 0 }
    }

    /// Samples that reached the limiter ceiling since the renderer was created (a sample at the
    /// ceiling means the limiter applied gain reduction there; a proxy for "would have clipped").
    pub fn limited_samples(&self) -> u64 {
        self.limited_samples
    }

    /// Maximum absolute output sample observed since the renderer was created.
    pub fn out_peak(&self) -> f32 {
        self.out_peak
    }

    /// Set the number of dynamic objects carried by the grouped batch element. Recreates the
    /// element when the count changes (object count is normally fixed for a stream).
    pub fn set_object_count(&mut self, count: usize) -> Result<()> {
        if count > self.max_objects {
            return Err(PipelineError::Render(format!(
                "object count {count} exceeds max_objects {}",
                self.max_objects
            )));
        }
        if count == self.object_count && (count == 0 || self.object_element_active) {
            return Ok(());
        }
        if self.object_element_active {
            self.rdr
                .remove_element(OBJ_ID)
                .map_err(|e| PipelineError::Render(format!("remove object element: {e:?}")))?;
            self.object_element_active = false;
        }
        if count > 0 {
            self.rdr
                .add_element(
                    self.group,
                    OBJ_ID,
                    &AudioElementConfig::ObjectBased(
                        ObjectBasedConfig::new_nsp_olr_batch(count).map_err(|e| {
                            PipelineError::Render(format!("ObjectBasedConfig: {e:?}"))
                        })?,
                    ),
                )
                .map_err(|e| PipelineError::Render(format!("add object element: {e:?}")))?;
            self.object_element_active = true;
        }
        self.object_count = count;
        self.positions = Vec::with_capacity(count);
        self.obj_scratch = vec![vec![0.0; self.frames.value() as usize]; count];
        Ok(())
    }

    /// ROAR Cartesian coordinates from the N03/ADM triplet \`[x=right, y=up, z=back]\`,
    /// normalised to the unit sphere (same axis swap as NSP \`olr_input.rs::coordinates\`).
    fn to_roar_cartesian(adm: [f64; 3]) -> Result<CartesianCoordinate> {
        let [x, y, z] = adm;
        let radius = (x * x + y * y + z * z).sqrt();
        let scale = if radius > 1e-9 { 1.0 / radius } else { 0.0 };
        CartesianCoordinate::new(
            (x * scale) as f32,
            (z * scale) as f32, // ROAR y = front = -ADM z
            (y * scale) as f32, // ROAR z = up = ADM y
        )
        .map_err(|e| PipelineError::Render(format!("CartesianCoordinate: {e:?}")))
    }

    /// Render one 480-frame decoded block. Returns planar channels in [\`roar_layout\`] order.
    pub fn render_block(&mut self, block: &DecodedBlock) -> Result<Vec<Vec<f32>>> {
        let frames = self.frames.value() as usize;
        if block.frames != frames {
            return Err(PipelineError::Render(format!(
                "block frames {} != renderer frames {frames}",
                block.frames
            )));
        }
        if block.bed.len() != BED_CHANNELS {
            return Err(PipelineError::Render(format!(
                "bed has {} channels, expected {BED_CHANNELS}",
                block.bed.len()
            )));
        }
        let object_count = if block.object_mode { block.objects.len() } else { 0 };
        self.set_object_count(object_count)?;

        // 1) positions + per-object gain for this block.
        self.positions.clear();
        if object_count > 0 {
            let metadata = block.metadata.as_ref();
            for (index, samples) in block.objects.iter().enumerate() {
                let attr = metadata.and_then(|m| m.dynamic().nth(index));
                let position = attr.and_then(|a| a.position).unwrap_or([0.0, 0.0, -1.0]);
                self.positions.push(Self::to_roar_cartesian(position)?);
                let gain = attr.map(|a| gain_db_to_linear(a.gain_db)).unwrap_or(1.0);
                let dst = &mut self.obj_scratch[index];
                if gain == 1.0 {
                    dst.copy_from_slice(&samples[..frames]);
                } else {
                    for (d, &s) in dst.iter_mut().zip(samples[..frames].iter()) {
                        *d = s * gain;
                    }
                }
            }
            self.rdr
                .update_element_positions(OBJ_ID, &ObjectPosition::Cartesian(self.positions.clone()), self.frames)
                .map_err(|e| PipelineError::Render(format!("update positions: {e:?}")))?;
        }

        // 2) inputs + output buffers.
        for (dst, src) in self.bed_scratch.iter_mut().zip(block.bed.iter()) {
            dst.copy_from_slice(&src[..frames]);
        }
        for channel in self.out_scratch.iter_mut() {
            channel.fill(0.0);
        }
        {
            let bed_refs: Vec<&[f32]> = self.bed_scratch.iter().map(|c| c.as_slice()).collect();
            let bed_buf = PlanarBufferRef::new(&bed_refs, BED_CHANNELS, self.frames)
                .map_err(|e| PipelineError::Render(format!("bed buffer: {e:?}")))?;
            let mut out_refs: Vec<&mut [f32]> =
                self.out_scratch.iter_mut().map(|c| c.as_mut_slice()).collect();
            let mut out_buf = PlanarBufferMut::new(&mut out_refs, self.out_channels, self.frames)
                .map_err(|e| PipelineError::Render(format!("out buffer: {e:?}")))?;
            if self.object_element_active {
                let obj_refs: Vec<&[f32]> = self.obj_scratch.iter().map(|c| c.as_slice()).collect();
                let obj_buf = PlanarBufferRef::new(&obj_refs, object_count, self.frames)
                    .map_err(|e| PipelineError::Render(format!("object buffer: {e:?}")))?;
                self.rdr
                    .render(&[(OBJ_ID, obj_buf), (BED_ID, bed_buf)], &mut out_buf)
                    .map_err(|e| PipelineError::Render(format!("render: {e:?}")))?;
            } else {
                self.rdr
                    .render(&[(BED_ID, bed_buf)], &mut out_buf)
                    .map_err(|e| PipelineError::Render(format!("render bed: {e:?}")))?;
            }
        }
        self.rendered_blocks += 1;
        // Measure the output (post-limiter) before handing it over: peak and how often the
        // limiter ceiling was reached. A sample at the ceiling marks gain reduction, i.e. content
        // that would have clipped without the limiter.
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
        // 3) hand the planar output to the caller; replace the scratch for the next block.
        Ok(std::mem::replace(
            &mut self.out_scratch,
            vec![vec![0.0; frames]; self.out_channels],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::BLOCK_FRAMES;
    use crate::oamd::{ObjectAttr, ObjectMetadata};
    use crate::layout::ChannelId;

    fn metadata_for(positions: &[[f64; 3]]) -> ObjectMetadata {
        ObjectMetadata {
            oamd_version: 0,
            object_count: positions.len(),
            bed_speakers: vec![],
            objects: positions
                .iter()
                .enumerate()
                .map(|(i, pos)| ObjectAttr {
                    index: i,
                    id: i as u32,
                    in_bed: false,
                    bed_channel: None,
                    position: Some(*pos),
                    gain_db: 0,
                    size: [0.0; 3],
                })
                .collect(),
            dynamic_objects: positions.len(),
            ramp_samples: 0,
            sample_offset: 0,
        }
    }

    fn sine_block(_freq: f32, objects: usize, positions: &[[f64; 3]]) -> DecodedBlock {
        let frames = BLOCK_FRAMES;
        let mut bed = vec![vec![0.0f32; frames]; 12];
        for (c, channel) in bed.iter_mut().enumerate() {
            let w = 2.0 * std::f32::consts::PI * (100.0 + 50.0 * c as f32) / 48000.0;
            for (i, s) in channel.iter_mut().enumerate() {
                *s = 0.25 * (w * i as f32).sin();
            }
        }
        let objects: Vec<Vec<f32>> = (0..objects)
            .map(|o| {
                let w = 2.0 * std::f32::consts::PI * (400.0 + 40.0 * o as f32) / 48000.0;
                (0..frames).map(|i| 0.5 * (w * i as f32).sin()).collect()
            })
            .collect();
        DecodedBlock {
            frames,
            sample_pos: 0,
            bed,
            objects,
            metadata: if positions.is_empty() { None } else { Some(metadata_for(positions)) },
            object_count: positions.len(),
            p2_channels: 8,
            p3_channels: 12,
            object_mode: !positions.is_empty(),
        }
    }

    fn channel_energy(out: &[Vec<f32>], layout: &LayoutSpec, channel: ChannelId) -> f32 {
        let index = layout.index_of(channel).unwrap();
        out[index].iter().map(|s| s * s).sum()
    }

    #[test]
    fn rejects_unsupported_layouts_and_bad_config() {
        assert!(ObjectRenderer::new(&RenderConfig::new("4.1.2", 480)).is_err());
        assert!(ObjectRenderer::new(&RenderConfig::new("7.1.4", 0)).is_err());
        assert!(ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).is_ok());
        let mut renderer = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        assert!(renderer.set_object_count(crate::decode::MAX_OBJECTS + 1).is_err());
    }

    #[test]
    fn bed_only_714_maps_every_channel_with_energy() {
        let mut renderer = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        let block = sine_block(0.0, 0, &[]);
        let out = renderer.render_block(&block).unwrap();
        assert_eq!(out.len(), 12);
        assert_eq!(renderer.roar_layout().id(), "7.1.4");
        for channel in renderer.roar_layout().channels() {
            let index = renderer.roar_layout().index_of(*channel).unwrap();
            let energy: f32 = out[index].iter().map(|s| s * s).sum();
            assert!(energy > 1.0, "bed channel {channel:?} silent (energy {energy})");
        }
        assert!(out.iter().all(|c| c.iter().all(|s| s.is_finite())));
    }

    #[test]
    fn single_object_localizes_left_vs_front() {
        let mut renderer = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        // ADM [-1,0,0] -> ROAR left.
        let left = sine_block(0.0, 1, &[[-1.0, 0.0, 0.0]]);
        let out = renderer.render_block(&left).unwrap();
        let layout = renderer.roar_layout().clone();
        let left_energy = channel_energy(&out, &layout, ChannelId::FL)
            + channel_energy(&out, &layout, ChannelId::LS)
            + channel_energy(&out, &layout, ChannelId::TFL);
        let right_energy = channel_energy(&out, &layout, ChannelId::FR)
            + channel_energy(&out, &layout, ChannelId::RS)
            + channel_energy(&out, &layout, ChannelId::TFR);
        assert!(left_energy > 2.0, "left object has no energy ({left_energy})");
        assert!(
            left_energy > right_energy * 1.8,
            "left/right localization failed: left={left_energy} right={right_energy}"
        );
    }

    #[test]
    fn twelve_objects_render_finite_energy_and_zero_object_mode() {
        let mut renderer = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        let positions: Vec<[f64; 3]> = (0..12)
            .map(|i| {
                let angle = -1.0 + 2.0 * i as f64 / 11.0;
                [angle, 0.0, -0.5]
            })
            .collect();
        let block = sine_block(0.0, 12, &positions);
        let out = renderer.render_block(&block).unwrap();
        assert_eq!(out.len(), 12);
        let total: f32 = out.iter().map(|c| c.iter().map(|s| s * s).sum::<f32>()).sum();
        assert!(total > 10.0, "12 objects produced almost no energy ({total})");
        assert_eq!(renderer.object_count(), 12);
        // Back to bed-only on the same renderer (element recreated).
        let bed_only = sine_block(0.0, 0, &[]);
        let out2 = renderer.render_block(&bed_only).unwrap();
        assert_eq!(out2.len(), 12);
        assert_eq!(renderer.object_count(), 0);
    }

    #[test]
    fn object_gain_is_applied() {
        let mut renderer = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        let mut block = sine_block(0.0, 1, &[[0.0, 0.0, -1.0]]);
        for channel in block.bed.iter_mut() {
            channel.fill(0.0); // isolate the object contribution from the bed
        }
        block.metadata.as_mut().unwrap().objects[0].gain_db = -6;
        let out = renderer.render_block(&block).unwrap();
        let total: f32 = out.iter().map(|c| c.iter().map(|s| s * s).sum::<f32>()).sum();
        let mut renderer2 = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        let mut unity = sine_block(0.0, 1, &[[0.0, 0.0, -1.0]]);
        for channel in unity.bed.iter_mut() {
            channel.fill(0.0);
        }
        unity.metadata.as_mut().unwrap().objects[0].gain_db = 0;
        let out_unity = renderer2.render_block(&unity).unwrap();
        let total_unity: f32 = out_unity.iter().map(|c| c.iter().map(|s| s * s).sum::<f32>()).sum();
        assert!(
            total < total_unity * 0.35 && total > total_unity * 0.15,
            "gain -6 dB not applied ({total} vs {total_unity})"
        );
    }

    #[test]
    fn limiter_keeps_output_within_ceiling() {
        let mut renderer = ObjectRenderer::new(&RenderConfig::new("7.1.4", 480)).unwrap();
        assert!(renderer.limiter_enabled());
        // +6 dB over full scale on every bed channel would clip without the limiter.
        let mut block = sine_block(0.0, 0, &[]);
        for channel in block.bed.iter_mut() {
            channel.fill(2.0);
        }
        let out = renderer.render_block(&block).unwrap();
        let peak = out
            .iter()
            .flat_map(|channel| channel.iter())
            .fold(0.0f32, |max, sample| max.max(sample.abs()));
        let ceiling = 0.891_251; // -1 dBFS
        assert!(peak <= ceiling + 1.0e-4, "limiter ceiling exceeded: peak {peak}");
        assert!(renderer.limited_samples() > 0, "limiter never engaged");
        assert!(renderer.out_peak() <= ceiling + 1.0e-4);
        assert!(renderer.limiter_delay() > 0);
        // Filtering every sample through a look-ahead limiter must stay finite.
        assert!(out.iter().all(|channel| channel.iter().all(|s| s.is_finite())));
    }

    #[test]
    fn supports_512_and_916_targets() {
        for target in ["5.1.2", "9.1.6"] {
            let mut renderer = ObjectRenderer::new(&RenderConfig::new(target, 480)).unwrap();
            let out = renderer.render_block(&sine_block(0.0, 1, &[[0.0, 0.0, -1.0]])).unwrap();
            assert_eq!(out.len(), renderer.target_layout().len());
            let total: f32 = out.iter().map(|c| c.iter().map(|s| s * s).sum::<f32>()).sum();
            assert!(total > 1.0, "{target} rendered no energy");
        }
    }
}
