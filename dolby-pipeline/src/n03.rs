//! N03: the internal decode -> render contract.
//!
//! Wire format copied from NSP Croksor (\`modules/croksor/src/input/capture_input.rs\`):
//! \`count * (32-byte property + frames * 4-byte f32 mono PCM)\`, little-endian properties.
//! Property fields: \`ident(u32) / frames(u32) / typ(u32) / flags(u32) / xyz[3](f32) / gain(f32)\`.
//!
//! * \`typ == 1\` dynamic object;
//! * \`typ ∈ {2,4,8,16,32,64,128,256,512,1024,2048,4096}\` = the 12 bed channels in ROAR
//!   Layout714 order (FL,FR,C,LFE,SL,SR,BL,BR,TFL,TFR,TBL,TBR).
//!
//! Coordinates are the ADM/object triplet used by the NSP engine: \`[x=right, y=up, z=back]\`,
//! normalised to the unit sphere by the renderer (NSP maps OLR axes as \`[x, -z, y]\`).
//! N03 is an **internal** contract (not a stable public ABI); it exists so decode, rendering and
//! tests can be decoupled and fed from either TrueHD/OAMD or a future DD+ (rustd) object source.

use crate::{PipelineError, Result};
use crate::layout::ChannelId;

pub const PROPERTY_BYTES: usize = 32;
pub const MAX_RECORDS: usize = 160;
pub const OBJECT_TYP: u32 = 1;
pub const FLAG_CREATE: u32 = 1;
pub const FLAG_POSITION: u32 = 4;

/// Bed type codes in ROAR Layout714 channel order.
pub const BED_TYPES: [u32; 12] = [2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

/// Bed layout 7.1.4 in ROAR/N03 order.
pub const BED_LAYOUT_714: [ChannelId; 12] = [
    ChannelId::FL,
    ChannelId::FR,
    ChannelId::FC,
    ChannelId::LFE,
    ChannelId::LS,
    ChannelId::RS,
    ChannelId::LRS,
    ChannelId::RRS,
    ChannelId::TFL,
    ChannelId::TFR,
    ChannelId::TBL,
    ChannelId::TBR,
];

/// One N03 record property.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct N03Record {
    pub ident: u32,
    pub frames: u32,
    pub typ: u32,
    pub flags: u32,
    /// ADM object position: x=right, y=up, z=back (normalised by the renderer).
    pub xyz: [f32; 3],
    /// Linear gain (1.0 = unity).
    pub gain: f32,
}

impl N03Record {
    pub fn is_object(&self) -> bool {
        self.typ == OBJECT_TYP
    }

    /// Index of the bed channel in \`BED_LAYOUT_714\`, if this is a bed record.
    pub fn bed_index(&self) -> Option<usize> {
        if self.typ == OBJECT_TYP {
            return None;
        }
        BED_TYPES.iter().position(|&t| t == self.typ)
    }

    pub fn channel(&self) -> Option<ChannelId> {
        self.bed_index().map(|i| BED_LAYOUT_714[i])
    }

    pub fn encode(&self) -> [u8; PROPERTY_BYTES] {
        let mut b = [0u8; PROPERTY_BYTES];
        b[0..4].copy_from_slice(&self.ident.to_le_bytes());
        b[4..8].copy_from_slice(&self.frames.to_le_bytes());
        b[8..12].copy_from_slice(&self.typ.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..20].copy_from_slice(&self.xyz[0].to_le_bytes());
        b[20..24].copy_from_slice(&self.xyz[1].to_le_bytes());
        b[24..28].copy_from_slice(&self.xyz[2].to_le_bytes());
        b[28..32].copy_from_slice(&self.gain.to_le_bytes());
        b
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < PROPERTY_BYTES {
            return Err(PipelineError::N03("record property needs 32 bytes".into()));
        }
        let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let f32_at = |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        Ok(Self {
            ident: u32_at(0),
            frames: u32_at(4),
            typ: u32_at(8),
            flags: u32_at(12),
            xyz: [f32_at(16), f32_at(20), f32_at(24)],
            gain: f32_at(28),
        })
    }
}

/// One 480-frame N03 block: records in order + mono PCM for each record.
#[derive(Clone, PartialEq, Debug)]
pub struct N03Block {
    pub frames: usize,
    pub records: Vec<N03Record>,
    /// Mono PCM, record-major: record r occupies \`pcm[r*frames .. (r+1)*frames]\`.
    pub pcm: Vec<f32>,
}

impl N03Block {
    pub fn new(frames: usize, records: Vec<N03Record>, pcm: Vec<f32>) -> Result<Self> {
        if frames == 0 || frames > 4096 {
            return Err(PipelineError::N03(format!("invalid frames {frames}")));
        }
        if records.len() > MAX_RECORDS {
            return Err(PipelineError::N03(format!(
                "{} records exceed N03 limit {MAX_RECORDS}",
                records.len()
            )));
        }
        if pcm.len() != records.len() * frames {
            return Err(PipelineError::N03(format!(
                "pcm has {} samples, expected {}",
                pcm.len(),
                records.len() * frames
            )));
        }
        for r in &records {
            if r.frames as usize != frames {
                return Err(PipelineError::N03(format!(
                    "record frames {} != block frames {frames}",
                    r.frames
                )));
            }
            if !(r.is_object() || r.bed_index().is_some()) {
                return Err(PipelineError::N03(format!("unsupported record type {}", r.typ)));
            }
        }
        Ok(Self { frames, records, pcm })
    }

    pub fn record_pcm(&self, index: usize) -> &[f32] {
        &self.pcm[index * self.frames..(index + 1) * self.frames]
    }

    pub fn object_records(&self) -> impl Iterator<Item = (usize, &N03Record)> {
        self.records.iter().enumerate().filter(|(_, r)| r.is_object())
    }

    pub fn bed_records(&self) -> impl Iterator<Item = (usize, &N03Record)> {
        self.records.iter().enumerate().filter(|(_, r)| !r.is_object())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.records.len() * (PROPERTY_BYTES + self.frames * 4));
        for (i, record) in self.records.iter().enumerate() {
            out.extend_from_slice(&record.encode());
            for sample in self.record_pcm(i) {
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
        out
    }

    /// Parse an N03 block; \`frames\` is fixed by the caller (480) because the wire format only
    /// carries \`frames\` inside each record.
    pub fn parse(bytes: &[u8], frames: usize) -> Result<Self> {
        if frames == 0 || bytes.len() % (PROPERTY_BYTES + frames * 4) != 0 {
            return Err(PipelineError::N03(format!(
                "N03 length {} is not a multiple of {} (frames={frames})",
                bytes.len(),
                PROPERTY_BYTES + frames * 4
            )));
        }
        let stride = PROPERTY_BYTES + frames * 4;
        let count = bytes.len() / stride;
        if count > MAX_RECORDS {
            return Err(PipelineError::N03(format!("{count} records exceed N03 limit {MAX_RECORDS}")));
        }
        let mut records = Vec::with_capacity(count);
        let mut pcm = Vec::with_capacity(count * frames);
        for i in 0..count {
            let at = i * stride;
            let record = N03Record::parse(&bytes[at..at + PROPERTY_BYTES])?;
            if record.frames as usize != frames {
                return Err(PipelineError::N03(format!(
                    "record {i} frames {} != {frames}",
                    record.frames
                )));
            }
            for c in 0..frames {
                let o = at + PROPERTY_BYTES + c * 4;
                pcm.push(f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]));
            }
            records.push(record);
        }
        Self::new(frames, records, pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(ident: u32, frames: usize, xyz: [f32; 3], gain: f32) -> N03Record {
        N03Record {
            ident,
            frames: frames as u32,
            typ: OBJECT_TYP,
            flags: FLAG_CREATE | FLAG_POSITION,
            xyz,
            gain,
        }
    }

    fn bed(frames: usize, channel: usize) -> N03Record {
        N03Record {
            ident: 0,
            frames: frames as u32,
            typ: BED_TYPES[channel],
            flags: FLAG_CREATE,
            xyz: [0.0; 3],
            gain: 1.0,
        }
    }

    #[test]
    fn record_round_trip() {
        let r = N03Record {
            ident: 42,
            frames: 480,
            typ: OBJECT_TYP,
            flags: FLAG_CREATE | FLAG_POSITION,
            xyz: [0.25, -0.5, 0.75],
            gain: 0.5,
        };
        let bytes = r.encode();
        assert_eq!(bytes.len(), PROPERTY_BYTES);
        assert_eq!(N03Record::parse(&bytes).unwrap(), r);
    }

    #[test]
    fn block_round_trip_with_bed_and_objects() {
        let frames = 480usize;
        let mut records = Vec::new();
        for ch in 0..12 {
            records.push(bed(frames, ch));
        }
        records.push(object(1, frames, [0.0, 0.0, 1.0], 1.0));
        records.push(object(2, frames, [1.0, 0.0, 0.0], 0.25));
        let pcm: Vec<f32> = (0..records.len() * frames).map(|i| (i % 97) as f32 * 1e-3).collect();
        let block = N03Block::new(frames, records, pcm).unwrap();
        let encoded = block.encode();
        assert_eq!(encoded.len(), 14 * (PROPERTY_BYTES + frames * 4));
        let parsed = N03Block::parse(&encoded, frames).unwrap();
        assert_eq!(parsed, block);
        assert_eq!(parsed.bed_records().count(), 12);
        assert_eq!(parsed.object_records().count(), 2);
        assert_eq!(parsed.records[0].channel(), Some(ChannelId::FL));
        assert_eq!(parsed.records[11].channel(), Some(ChannelId::TBR));
    }

    #[test]
    fn rejects_bad_records() {
        let frames = 480usize;
        assert!(N03Block::new(frames, vec![], vec![1.0]).is_err());
        assert!(N03Block::parse(&[0u8; 7], frames).is_err());
        let mut bad = bed(frames, 0);
        bad.typ = 3; // not a valid bed type
        assert!(N03Block::new(frames, vec![bad], vec![0.0; frames]).is_err());
        let mut wrong_frames = bed(240, 0);
        wrong_frames.frames = 240;
        let _ = wrong_frames;
        let mut records = Vec::new();
        for ch in 0..12 {
            records.push(bed(frames, ch));
        }
        records.push(object(1, 240, [0.0; 3], 1.0));
        assert!(N03Block::new(frames, records, vec![0.0; 14 * frames]).is_err());
    }
}
