//! AES67Bridge Dolby object pipeline: live.bits -> TrueHD ES -> decode/OAMD -> N03 -> ROAR ->
//! 480-frame planar PCM blocks for the AES67 daemon.
//!
//! Scope (product decision 2026-11-24):
//! * in the bridge: live.bits boundary handling (pass-through, see [\`bits\`]), TrueHD/DD+ decoding,
//!   OAMD object attributes, the N03 decode/render contract, ROAR object rendering, channel
//!   identity/layout intersection, 480-frame block output (44.1 kHz -> 48 kHz SRC stays in the
//!   transport/daemon layer, not here);
//! * **NO DSP in the bridge**: no EQ, crossover, delay, room correction, upmix/downmix, DRC,
//!   dialnorm or limiting. A reserved post-render per-output-channel insertion point exists
//!   ([\`blocks::BlockProcessor\`], default empty chain) but no processor is implemented or enabled.
//! * layout: target = declared max layout; actual = target ∩ source channel labels; extra source
//!   channels dropped with a warning; missing target channels not filled; no downmix. SDP/status
//!   must publish [\`blocks::Block::actual\`].
//!
//! Third-party code is vendored under \`vendor/\` (truehd/eac3/oamd Apache-2.0; aom-roar
//! BSD-3-Clause-Clear + AOM patent license). See \`vendor/ORIGINS.json\`; licenses are recorded,
//! not judged.

pub mod bits;
pub mod blocks;
pub mod decode;
pub mod layout;
pub mod n03;
pub mod oamd;
pub mod pcm;
pub mod render;

/// Pipeline error type.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("bits: {0}")]
    Bits(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("oamd: {0}")]
    Oamd(String),
    #[error("n03: {0}")]
    N03(String),
    #[error("render: {0}")]
    Render(String),
    #[error("layout: {0}")]
    Layout(String),
    #[error("config: {0}")]
    Config(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("io: {0}")]
    Io(String),
}

impl From<aom_roar::OarError> for PipelineError {
    fn from(error: aom_roar::OarError) -> Self {
        PipelineError::Render(format!("{error:?}"))
    }
}

pub type Result<T> = std::result::Result<T, PipelineError>;

pub use bits::{Codec, EsNormalizer, TrueHdAuHeader};
pub use blocks::{
    Block, BlockChain, BlockPipeline, BlockProcessor, IdentityProcessor, ObjectPipeline, RenderStats,
};
pub use decode::{AuAssembler, DecodeMode, DecodeStats, DecodedAu, DecodedBlock, TrueHdDecoder, AU_SAMPLES, BLOCK_FRAMES, MAX_OBJECTS, SAMPLE_RATE};
pub use layout::{intersect, ChannelId, LayoutAdapter, LayoutIntersection, LayoutMode, LayoutSpec, IntersectionAdapter};
pub use n03::{N03Block, N03Record, BED_LAYOUT_714, BED_TYPES, PROPERTY_BYTES};
pub use oamd::{from_oamd, gain_db_to_linear, ObjectAttr, ObjectMetadata};
pub use pcm::{PcmConfig, PcmRenderer};
pub use render::{ObjectRenderer, RenderConfig};

/// Crate version string (for status/diagnostics).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
