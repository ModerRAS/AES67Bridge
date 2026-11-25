//! 480-frame output block contract, layout adaptation and the (empty) DSP insertion point.
//!
//! Chain position (fixed by the product scope):
//! \`\`\`
//! decode -> ROAR render (target layout) -> LayoutAdapter (strict intersection, target order)
//!        -> [BlockProcessor chain: default EMPTY] -> AES67 daemon (L24 packing / multi-stream)
//! \`\`\`
//!
//! * No DSP is implemented in the bridge. \`BlockProcessor\` is the reserved post-render,
//!   per-output-channel insertion point (EQ / delay / crossover live downstream, close to the
//!   speakers). The default chain is empty and must stay byte-transparent.
//! * Layout policy is strict: actual layout = target ∩ source channel labels; source channels
//!   outside target are dropped with a warning; target channels missing from source are not
//!   filled; no downmix/upmix. SDP/status must publish \`Block::actual\`.
//! * Upmix/downmix are reserved interfaces ([\`crate::layout::UpmixAdapter\`] /
//!   [\`crate::layout::DownmixAdapter\`]) and a reserved config value
//!   (\`layout_mode=strict|upmix|downmix|auto\`), not implemented and not enabled.

use crate::decode::DecodedBlock;
use crate::layout::{intersect, LayoutAdapter, LayoutMode, LayoutSpec, IntersectionAdapter};
use crate::render::ObjectRenderer;
use crate::{PipelineError, Result};

/// One published 480-frame block.
#[derive(Clone, Debug)]
pub struct Block {
    /// Monotonic block sequence number.
    pub seq: u64,
    /// Absolute sample position of the block start (48 kHz samples).
    pub sample_pos: u64,
    pub rate: u32,
    pub frames: usize,
    /// Declared maximum layout (what the backend was configured with).
    pub target: LayoutSpec,
    /// Source/content layout used for the intersection decision.
    pub source: LayoutSpec,
    /// Actual layout to publish in SDP/status; \`channels\` matches this layout.
    pub actual: LayoutSpec,
    pub object_count: usize,
    /// Planar channels in actual-layout order.
    pub channels: Vec<Vec<f32>>,
}

impl Block {
    /// SDP/status channel label string, e.g. \`"L,R,C,LFE,Ls,Rs"\`.
    pub fn channel_labels(&self) -> String {
        self.actual.label_csv()
    }
}

/// Reserved post-render DSP slot. The default implementation set is empty; processors run after
/// layout adaptation and before block submission, on the actual (published) layout.
pub trait BlockProcessor {
    fn process(&mut self, block: &mut Block) -> Result<()>;
}

/// Empty processor: proves the chain is byte-transparent.
#[derive(Default, Clone, Copy, Debug)]
pub struct IdentityProcessor;

impl BlockProcessor for IdentityProcessor {
    fn process(&mut self, _block: &mut Block) -> Result<()> {
        Ok(())
    }
}

/// Ordered processor chain. Empty by default.
#[derive(Default)]
pub struct BlockChain {
    processors: Vec<Box<dyn BlockProcessor>>,
}

impl BlockChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.processors.len()
    }

    pub fn push(&mut self, processor: Box<dyn BlockProcessor>) {
        self.processors.push(processor);
    }

    pub fn process(&mut self, block: &mut Block) -> Result<()> {
        for processor in self.processors.iter_mut() {
            processor.process(block)?;
        }
        Ok(())
    }
}

/// Builds published blocks from ROAR planar output. Owns the strict layout adapter and the
/// reserved DSP chain.
pub struct BlockPipeline {
    target: LayoutSpec,
    /// Router output order (same channel set as target, ROAR speaker order).
    roar_layout: LayoutSpec,
    frames: usize,
    seq: u64,
    adapter: Box<dyn LayoutAdapter>,
    processors: BlockChain,
    zero_channel: Vec<f32>,
}

impl BlockPipeline {
    pub fn new(target: &LayoutSpec, roar_layout: &LayoutSpec, frames: usize) -> Result<Self> {
        if frames == 0 {
            return Err(PipelineError::Config("block frames must be > 0".into()));
        }
        if target.len() != roar_layout.len() {
            return Err(PipelineError::Layout(format!(
                "target {} has {} channels but ROAR output {} has {}",
                target.id(),
                target.len(),
                roar_layout.id(),
                roar_layout.len()
            )));
        }
        Ok(Self {
            target: target.clone(),
            roar_layout: roar_layout.clone(),
            frames,
            seq: 0,
            adapter: Box::new(IntersectionAdapter::new()),
            processors: BlockChain::new(),
            zero_channel: vec![0.0; frames],
        })
    }

    /// Reserve \`layout_mode\` from config. Only \`strict\` is valid today; other values are
    /// rejected (never silently enabled).
    pub fn set_layout_mode(&mut self, mode: LayoutMode) -> Result<()> {
        match mode {
            LayoutMode::Strict => Ok(()),
            other => Err(PipelineError::Unsupported(format!(
                "layout_mode={other:?} is reserved and not implemented"
            ))),
        }
    }

    pub fn processors_mut(&mut self) -> &mut BlockChain {
        &mut self.processors
    }

    pub fn target_layout(&self) -> &LayoutSpec {
        &self.target
    }

    pub fn oracle(&self) -> LayoutIntersectionSummary {
        LayoutIntersectionSummary { target: self.target.clone() }
    }

    /// Build one block.
    ///
    /// \`rendered\` must be planar channels in [\`self.roar_layout\`] order. \`source\` describes the
    /// content layout (for object content this is the target layout: objects can use every
    /// target speaker; for bed-only content it is the bed presentation layout, e.g. 7.1).
    pub fn build(
        &mut self,
        rendered: Vec<Vec<f32>>,
        source: &LayoutSpec,
        sample_pos: u64,
        object_count: usize,
    ) -> Result<Block> {
        if rendered.len() != self.roar_layout.len() {
            return Err(PipelineError::Layout(format!(
                "rendered {} channels, ROAR layout {} expects {}",
                rendered.len(),
                self.roar_layout.id(),
                self.roar_layout.len()
            )));
        }
        if rendered.iter().any(|c| c.len() != self.frames) {
            return Err(PipelineError::Layout("rendered channel length mismatch".into()));
        }
        // Input for the adapter: channels in source order, borrowed from the renderer output where
        // the channel exists; source channels the renderer cannot produce (outside target) feed
        // silence — the adapter drops them anyway (target intersection) and warns.
        let mut input_refs: Vec<&[f32]> = Vec::with_capacity(source.len());
        let mut missing = Vec::new();
        for channel in source.channels() {
            match self.roar_layout.index_of(*channel) {
                Some(index) => input_refs.push(rendered[index].as_slice()),
                None => {
                    missing.push(*channel);
                    input_refs.push(self.zero_channel.as_slice());
                }
            }
        }
        if !missing.is_empty() {
            log::debug!("renderer cannot produce source channels {missing:?} (outside target)");
        }
        let actual_preview = intersect(&self.target, source);
        let actual_len = actual_preview.actual.len();
        let frames = self.frames;
        let mut out_data: Vec<Vec<f32>> = (0..actual_len.max(1)).map(|_| vec![0.0f32; frames]).collect();
        let mut out_refs: Vec<&mut [f32]> = out_data.iter_mut().map(|c| c.as_mut_slice()).collect();
        let actual = self.adapter.adapt(source, &self.target, &input_refs, &mut out_refs)?;
        let mut actual = actual;
        // Normalise the id: intersect() already returns known ids, keep adapter result as-is.
        if actual.len() != out_data.len() && out_data.len() != 1 {
            return Err(PipelineError::Layout(format!(
                "adapter returned {} channels but wrote {}",
                actual.id(),
                out_data.len()
            )));
        }
        actual = LayoutSpec::new(actual.id().to_string(), actual.channels().to_vec());
        out_data.truncate(actual.len());
        let mut block = Block {
            seq: self.seq,
            sample_pos,
            rate: crate::decode::SAMPLE_RATE,
            frames,
            target: self.target.clone(),
            source: source.clone(),
            actual,
            object_count,
            channels: out_data,
        };
        self.processors.process(&mut block)?;
        self.seq += 1;
        Ok(block)
    }
}

/// Small helper for callers that only need the intersection summary.
#[derive(Clone, Debug)]
pub struct LayoutIntersectionSummary {
    pub target: LayoutSpec,
}

/// Renderer output statistics (limiter + peak metering), for daemon status/logging.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    /// ROAR look-ahead peak limiter is enabled on the output.
    pub limiter: bool,
    /// Limiter look-ahead delay in samples per channel (0 when disabled).
    pub limiter_delay_samples: usize,
    /// Output samples that reached the limiter ceiling (would have clipped without it).
    pub limited_samples: u64,
    /// Maximum absolute output sample seen so far (linear, 1.0 = 0 dBFS).
    pub out_peak: f32,
}

/// End-to-end object pipeline: TrueHD decoder + 480-frame assembler + ROAR renderer + block
/// pipeline. This is the integration point the daemon uses.
pub struct ObjectPipeline {
    decoder: crate::decode::TrueHdDecoder,
    assembler: crate::decode::AuAssembler,
    renderer: ObjectRenderer,
    blocks: BlockPipeline,
    es: crate::bits::EsNormalizer,
    pub served_blocks: u64,
    /// Latest dialogue-normalisation metadata from the decoder.
    dialnorm: crate::decode::DialnormInfo,
    /// Gain currently applied to bed/objects before rendering (dB, 0 = transparent).
    dialnorm_gain_db: f32,
    dialnorm_enabled: bool,
    /// User output trim in dB (daemon --gain-db / gain_db), applied before the limiter.
    user_gain_db: f32,
    /// Combined gain currently configured on the renderer (dialnorm + user trim).
    applied_gain_db: f32,
}

impl ObjectPipeline {
    pub fn new(target: &str) -> Result<Self> {
        Self::new_with(target, -1.0)
    }

    /// Like new(), but with a configurable limiter ceiling in dBFS (daemon --limiter-db).
    pub fn new_with(target: &str, limiter_db: f32) -> Result<Self> {
        let target_layout = LayoutSpec::known(target).ok_or_else(|| {
            PipelineError::Unsupported(format!("unknown target layout {target:?}"))
        })?;
        let mut render_config =
            crate::render::RenderConfig::new(target, crate::decode::BLOCK_FRAMES);
        render_config.limiter_threshold_db = limiter_db;
        let renderer = ObjectRenderer::new(&render_config)?;
        let blocks = BlockPipeline::new(
            &target_layout,
            renderer.roar_layout(),
            crate::decode::BLOCK_FRAMES,
        )?;
        Ok(Self {
            decoder: crate::decode::TrueHdDecoder::new(),
            assembler: crate::decode::AuAssembler::new(crate::decode::BLOCK_FRAMES),
            renderer,
            blocks,
            es: crate::bits::EsNormalizer::new(),
            served_blocks: 0,
            dialnorm: crate::decode::DialnormInfo::default(),
            dialnorm_gain_db: 0.0,
            dialnorm_enabled: true,
            user_gain_db: 0.0,
            applied_gain_db: 0.0,
        })
    }

    pub fn target_layout(&self) -> &LayoutSpec {
        self.blocks.target_layout()
    }

    pub fn processors_mut(&mut self) -> &mut BlockChain {
        self.blocks.processors_mut()
    }

    /// Feed live.bits bytes (TrueHD). Returns every completed published block.
    pub fn push_bits(&mut self, bytes: &[u8]) -> Result<Vec<Block>> {
        let es = self.es.push(bytes);
        let aus = self.decoder.push_es(es);
        let mut out = Vec::new();
        for au in aus {
            if let Some(decoded) = self.assembler.push(au) {
                out.push(self.render_decoded(&decoded)?);
            }
        }
        Ok(out)
    }

    /// Render + publish one decoded 480-frame block.
    /// Enable/disable the dialogue-normalisation constant gain (default on). Turning it off
    /// restores a transparent renderer even when the stream carries a dialnorm code.
    pub fn set_dialnorm_enabled(&mut self, enabled: bool) {
        self.dialnorm_enabled = enabled;
        if !enabled {
            self.dialnorm_gain_db = 0.0;
        }
        self.refresh_input_gain();
    }

    /// User output trim in dB applied together with dialnorm, before the limiter.
    pub fn set_user_gain_db(&mut self, db: f32) {
        self.user_gain_db = db.clamp(-60.0, 12.0);
        self.refresh_input_gain();
    }

    pub fn user_gain_db(&self) -> f32 {
        self.user_gain_db
    }

    /// Combined gain currently configured on the renderer (dialnorm + user trim, dB).
    pub fn applied_gain_db(&self) -> f32 {
        self.applied_gain_db
    }

    fn refresh_input_gain(&mut self) {
        let dialnorm = if self.dialnorm_enabled {
            self.dialnorm_gain_db
        } else {
            0.0
        };
        let total = dialnorm + self.user_gain_db;
        if (total - self.applied_gain_db).abs() > 1.0e-6 {
            self.applied_gain_db = total;
            self.renderer.set_input_gain(10f32.powf(total / 20.0));
        }
    }

    /// Latest dialogue-normalisation metadata and the gain currently applied before rendering.
    pub fn dialnorm(&self) -> crate::decode::DialnormInfo {
        self.dialnorm
    }

    pub fn dialnorm_gain_db(&self) -> f32 {
        self.dialnorm_gain_db
    }

    pub fn render_decoded(&mut self, decoded: &DecodedBlock) -> Result<Block> {
        if self.dialnorm_enabled {
            self.dialnorm = self.decoder.dialnorm();
            let channels = if decoded.object_mode && decoded.object_count > 0 {
                decoded.p3_channels
            } else {
                decoded.p2_channels
            };
            self.dialnorm_gain_db = self.dialnorm.gain_db(channels);
        }
        self.refresh_input_gain();
        let source = self.source_layout(decoded);
        let rendered = self.renderer.render_block(decoded)?;
        let sample_pos = decoded.sample_pos;
        let object_count = decoded.object_count;
        let block = self.blocks.build(rendered, &source, sample_pos, object_count)?;
        self.served_blocks += 1;
        Ok(block)
    }

    /// Source/content layout for the intersection decision:
    /// * object content: full target layout (objects can be rendered to every target speaker),
    /// * bed-only content: the bed presentation layout (5.1 for 6ch, 7.1 for 8ch).
    fn source_layout(&self, decoded: &DecodedBlock) -> LayoutSpec {
        if decoded.object_mode && decoded.object_count > 0 {
            return self.blocks.target_layout().clone();
        }
        match decoded.p2_channels {
            6 => LayoutSpec::known("5.1").unwrap_or_else(|| LayoutSpec::known("5.1").unwrap()),
            _ => LayoutSpec::known("7.1").unwrap_or_else(|| LayoutSpec::known("7.1").unwrap()),
        }
    }

    pub fn decode_stats(&self) -> crate::decode::DecodeStats {
        self.decoder.stats()
    }

    /// Renderer/limiter statistics for the daemon status line.
    pub fn render_stats(&self) -> RenderStats {
        RenderStats {
            limiter: self.renderer.limiter_enabled(),
            limiter_delay_samples: self.renderer.limiter_delay(),
            limited_samples: self.renderer.limited_samples(),
            out_peak: self.renderer.out_peak(),
        }
    }

    pub fn es_codec(&self) -> crate::bits::Codec {
        self.es.codec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ChannelId;

    fn marker_channel(index: usize, frames: usize) -> Vec<f32> {
        vec![index as f32; frames]
    }

    fn roar_rendered(layout_id: &str) -> (LayoutSpec, Vec<Vec<f32>>) {
        let layout = LayoutSpec::roar(layout_id).unwrap();
        let rendered = (0..layout.len()).map(|i| marker_channel(i, 4)).collect();
        (layout, rendered)
    }

    #[test]
    fn intersection_512_target_with_51_source_is_51() {
        let target = LayoutSpec::known("5.1.2").unwrap();
        let (roar, rendered) = roar_rendered("5.1.2");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        let source = LayoutSpec::known("5.1").unwrap();
        let block = pipeline.build(rendered, &source, 0, 0).unwrap();
        assert_eq!(block.actual.id(), "5.1");
        assert_eq!(block.channels.len(), 6);
        assert_eq!(block.channel_labels(), "L,R,C,LFE,Ls,Rs");
        assert_eq!(block.target.id(), "5.1.2");
    }

    #[test]
    fn intersection_712_target_with_71_source_is_71() {
        let target = LayoutSpec::known("7.1.2").unwrap();
        let (roar, rendered) = roar_rendered("7.1.2");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        let source = LayoutSpec::known("7.1").unwrap();
        let block = pipeline.build(rendered, &source, 0, 0).unwrap();
        assert_eq!(block.actual.id(), "7.1");
        assert_eq!(block.channels.len(), 8);
        assert_eq!(block.channel_labels(), "L,R,C,LFE,Ls,Rs,Lb,Rb");
    }

    #[test]
    fn intersection_712_target_with_51_source_is_51() {
        let target = LayoutSpec::known("7.1.2").unwrap();
        let (roar, rendered) = roar_rendered("7.1.2");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        let source = LayoutSpec::known("5.1").unwrap();
        let block = pipeline.build(rendered, &source, 0, 0).unwrap();
        assert_eq!(block.actual.id(), "5.1");
        assert_eq!(block.channels.len(), 6);
        assert_eq!(block.source.id(), "5.1");
    }

    #[test]
    fn smaller_target_drops_extra_source_channels_without_downmix() {
        let target = LayoutSpec::known("5.1").unwrap();
        let (roar, rendered) = roar_rendered("5.1");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        let source = LayoutSpec::known("7.1.4").unwrap();
        let block = pipeline.build(rendered, &source, 0, 0).unwrap();
        assert_eq!(block.actual.id(), "5.1");
        assert_eq!(block.channels.len(), 6);
        // Strict = channel selection, not a downmix: L/R/C/LFE/Ls/Rs carry only their own content.
        // (Rendered channel index 0 is L in ROAR 7.1.4 order too, so the marker survives.)
        assert_eq!(block.channels[0][0], 0.0);
    }

    #[test]
    fn object_content_uses_full_target_layout() {
        let target = LayoutSpec::known("7.1.4").unwrap();
        let (roar, rendered) = roar_rendered("7.1.4");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        let block = pipeline.build(rendered, &target.clone(), 0, 12).unwrap();
        assert_eq!(block.actual.id(), "7.1.4");
        assert_eq!(block.channels.len(), 12);
        assert_eq!(block.object_count, 12);
    }

    #[test]
    fn empty_chain_and_identity_chain_are_byte_identical() {
        let target = LayoutSpec::known("7.1.4").unwrap();
        let (roar, rendered) = roar_rendered("7.1.4");
        let mut empty = BlockPipeline::new(&target, &roar, 4).unwrap();
        let block_empty = empty.build(rendered.clone(), &target.clone(), 0, 3).unwrap();
        let mut identity = BlockPipeline::new(&target, &roar, 4).unwrap();
        identity.processors_mut().push(Box::new(IdentityProcessor));
        let block_identity = identity.build(rendered, &target.clone(), 0, 3).unwrap();
        assert_eq!(block_empty.channels, block_identity.channels);
        assert_eq!(block_empty.actual.id(), block_identity.actual.id());
        // Two identity processors are also transparent.
        let (_, rendered) = roar_rendered("7.1.4");
        let mut double = BlockPipeline::new(&target, &roar, 4).unwrap();
        double.processors_mut().push(Box::new(IdentityProcessor));
        double.processors_mut().push(Box::new(IdentityProcessor));
        let block_double = double.build(rendered, &target.clone(), 0, 3).unwrap();
        assert_eq!(block_empty.channels, block_double.channels);
    }

    #[test]
    fn processor_runs_after_layout_and_sees_actual_layout() {
        struct ZeroLeft;
        impl BlockProcessor for ZeroLeft {
            fn process(&mut self, block: &mut Block) -> Result<()> {
                assert!(!block.actual.is_empty());
                let left = block.actual.index_of(ChannelId::FL).unwrap();
                block.channels[left].fill(0.0);
                Ok(())
            }
        }
        let target = LayoutSpec::known("5.1.2").unwrap();
        let (roar, rendered) = roar_rendered("5.1.2");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        pipeline.processors_mut().push(Box::new(ZeroLeft));
        let source = LayoutSpec::known("5.1").unwrap();
        let block = pipeline.build(rendered, &source, 0, 0).unwrap();
        assert_eq!(block.actual.id(), "5.1");
        assert!(block.channels[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn layout_mode_is_strict_only() {
        let target = LayoutSpec::known("7.1.4").unwrap();
        let roar = LayoutSpec::roar("7.1.4").unwrap();
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        assert!(pipeline.set_layout_mode(LayoutMode::Strict).is_ok());
        assert!(pipeline.set_layout_mode(LayoutMode::Upmix).is_err());
        assert!(pipeline.set_layout_mode(LayoutMode::Downmix).is_err());
        assert!(pipeline.set_layout_mode(LayoutMode::Auto).is_err());
    }

    #[test]
    fn block_seq_and_sample_pos_advance() {
        let target = LayoutSpec::known("7.1.4").unwrap();
        let (roar, rendered) = roar_rendered("7.1.4");
        let mut pipeline = BlockPipeline::new(&target, &roar, 4).unwrap();
        let b0 = pipeline.build(rendered.clone(), &target.clone(), 0, 0).unwrap();
        let b1 = pipeline.build(rendered, &target.clone(), 480, 0).unwrap();
        assert_eq!(b0.seq, 0);
        assert_eq!(b1.seq, 1);
        assert_eq!(b1.sample_pos, 480);
    }
}
