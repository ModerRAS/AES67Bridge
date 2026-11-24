//! OAMD (Object Audio Metadata) -> object attributes.
//!
//! The TrueHD crate parses the OAMD evolution payload into \`oamd::ObjectAudioMetadataPayload\`
//! (presentation 3 only). This module extracts the attributes the renderer needs:
//! position, gain (dB), size, ramp duration and bed assignment.
//!
//! Coordinate conventions:
//! * \`get_damf_pos()\` returns DAMF-normalised \`[x=right, y=front, z=up]\` in [-1, 1].
//! * N03 stores the NSP/ADM triplet \`[x=right, y=up, z=back]\`; the renderer maps it to ROAR's
//!   Cartesian \`[x=right, y=front, z=up]\` as \`[x, -z, y]\` (the same mapping NSP's
//!   \`croksor/src/render/olr_input.rs:28-32\` uses). Conversion here: \`adm = [damf_x, damf_z, -damf_y]\`.

use crate::{PipelineError, Result};
use crate::layout::ChannelId;
use oamd::ObjectAudioMetadataPayload;

/// OAMD speaker index -> canonical channel id (order of \`oamd::SpeakerLabels\`; same as NSP's
/// \`bridge-family-dolby/src/labels.rs::oamd_speaker_to_label\`).
pub fn oamd_speaker_to_channel(index: usize) -> Option<ChannelId> {
    Some(match index {
        0 => ChannelId::FL,
        1 => ChannelId::FR,
        2 => ChannelId::FC,
        3 => ChannelId::LFE,
        4 => ChannelId::LS,
        5 => ChannelId::RS,
        6 => ChannelId::LRS,
        7 => ChannelId::RRS,
        8 => ChannelId::TFL,
        9 => ChannelId::TFR,
        10 => ChannelId::TSL,
        11 => ChannelId::TSR,
        12 => ChannelId::TBL,
        13 => ChannelId::TBR,
        14 => ChannelId::LW,
        15 => ChannelId::RW,
        16 => ChannelId::LFE2,
        _ => return None,
    })
}

/// One OAMD object (bed member or dynamic object), indexed by its presentation-3 channel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ObjectAttr {
    /// OAMD object index == presentation 3 channel index.
    pub index: usize,
    /// Dynamic object id (sequential over non-bed objects, stable per stream).
    pub id: u32,
    /// True when the object is anchored to a fixed bed speaker.
    pub in_bed: bool,
    /// Bed speaker for bed members (None for dynamic objects).
    pub bed_channel: Option<ChannelId>,
    /// ADM position \`[x=right, y=up, z=back]\`; \`None\` for bed members.
    pub position: Option<[f64; 3]>,
    /// OAMD object gain in dB; -128 (oamd::GAIN_MINUS_INFINITY) means -inf.
    pub gain_db: i8,
    /// Object size per axis (width, depth, height), 0..1 each.
    pub size: [f64; 3],
}

/// Decoded OAMD attributes for one access unit (metadata applies at its sample offset).
#[derive(Clone, PartialEq, Debug)]
pub struct ObjectMetadata {
    pub oamd_version: u8,
    /// OAMD object_count (== presentation-3 channel count).
    pub object_count: usize,
    /// Bed speakers in presentation-3 bed channel order.
    pub bed_speakers: Vec<ChannelId>,
    /// All objects, indexed by OAMD object index.
    pub objects: Vec<ObjectAttr>,
    pub dynamic_objects: usize,
    /// Ramp duration in samples for this metadata update.
    pub ramp_samples: u16,
    /// Sample offset inside the access unit.
    pub sample_offset: usize,
}

impl ObjectMetadata {
    pub fn dynamic(&self) -> impl Iterator<Item = &ObjectAttr> {
        self.objects.iter().filter(|o| !o.in_bed)
    }

    /// Presentation-3 channel index -> bed speaker for bed members.
    pub fn bed_channels(&self) -> Vec<(usize, ChannelId)> {
        self.objects
            .iter()
            .filter(|o| o.in_bed)
            .filter_map(|o| o.bed_channel.map(|c| (o.index, c)))
            .collect()
    }

    pub fn dynamic_count(&self) -> usize {
        self.dynamic_objects
    }
}

/// Convert an OAMD payload into renderer attributes.
pub fn from_oamd(payload: &ObjectAudioMetadataPayload) -> Result<ObjectMetadata> {
    let beds: Vec<usize> = payload
        .program_assignment
        .bed_assignment
        .first()
        .map(|bed| bed.to_index_vec())
        .unwrap_or_default();
    let mut bed_speakers = Vec::with_capacity(beds.len());
    for &speaker in &beds {
        bed_speakers.push(oamd_speaker_to_channel(speaker).ok_or_else(|| {
            PipelineError::Oamd(format!("unmapped OAMD bed speaker index {speaker}"))
        })?);
    }

    let damf_pos = payload.get_damf_pos();
    let (ramp_samples, sample_offset) = payload
        .object_element
        .as_ref()
        .map(|element| {
            let ramp = element
                .md_update_info
                .block_update_info
                .first()
                .map(|b| b.ramp_duration)
                .unwrap_or(0);
            (ramp, element.md_update_info.sample_offset)
        })
        .unwrap_or((0, 0));

    let mut objects = Vec::with_capacity(payload.object_count);
    let mut dynamic_objects = 0usize;
    for index in 0..payload.object_count {
        let block = payload
            .object_element
            .as_ref()
            .and_then(|element| element.object_data.get(index))
            .and_then(|blocks| blocks.first());
        let Some(block) = block else {
            // Missing object data: treat as inactive dynamic object with -inf gain.
            objects.push(ObjectAttr {
                index,
                id: dynamic_objects as u32,
                in_bed: index < bed_speakers.len(),
                bed_channel: bed_speakers.get(index).copied(),
                position: None,
                gain_db: i8::MIN,
                size: [0.0; 3],
            });
            if index >= bed_speakers.len() {
                dynamic_objects += 1;
            }
            continue;
        };
        let in_bed = block.b_object_in_bed_or_isf;
        let bed_channel = if in_bed { bed_speakers.get(index).copied() } else { None };
        let position = if in_bed {
            None
        } else {
            damf_pos.get(index).and_then(|blocks| blocks.first()).map(|p| {
                // DAMF [right, front, up] -> ADM [right, up, back]
                [p[0], p[2], -p[1]]
            })
        };
        let id = if in_bed { index as u32 } else { dynamic_objects as u32 };
        if !in_bed {
            dynamic_objects += 1;
        }
        objects.push(ObjectAttr {
            index,
            id,
            in_bed,
            bed_channel,
            position,
            gain_db: block.object_basic_info.object_gain,
            size: block.object_render_info.object_size,
        });
    }

    Ok(ObjectMetadata {
        oamd_version: payload.oamd_version,
        object_count: payload.object_count,
        bed_speakers,
        objects,
        dynamic_objects,
        ramp_samples,
        sample_offset,
    })
}

/// OAMD gain code -> linear gain. -inf code maps to 0.0.
pub fn gain_db_to_linear(gain_db: i8) -> f32 {
    if gain_db <= -128 {
        0.0
    } else {
        10f32.powf(gain_db as f32 / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oamd::{
        BedAssignment, BlockUpdateInfo, MDUpdateInfo, ObjectBasicInfo, ObjectElement, ObjectInfoBlock,
        ObjectRenderInfo, ProgramAssignment,
    };

    fn render_info(x: f64, y: f64, z: f64) -> ObjectRenderInfo {
        let mut r = ObjectRenderInfo::default();
        r.pos3d = [x, y, z];
        r.object_size = [0.1, 0.2, 0.3];
        r
    }

    fn object_block(in_bed: bool, gain_db: i8) -> ObjectInfoBlock {
        ObjectInfoBlock {
            b_object_not_active: false,
            object_basic_info: ObjectBasicInfo { object_gain: gain_db, object_priority: 1.0 },
            b_object_in_bed_or_isf: in_bed,
            object_render_info: ObjectRenderInfo::default(),
        }
    }

    fn payload(objects: Vec<Vec<ObjectInfoBlock>>, bed_std: u16) -> ObjectAudioMetadataPayload {
        let mut p = ObjectAudioMetadataPayload::default();
        p.oamd_version = 0;
        p.object_count = objects.len();
        let mut program = ProgramAssignment::default();
        program.bed_assignment = vec![BedAssignment::from_std(bed_std)];
        program.num_bed_objects = 2;
        program.num_dynamic_objects = objects.len().saturating_sub(2);
        p.program_assignment = program;
        let mut element = ObjectElement::default();
        element.md_update_info = MDUpdateInfo {
            sample_offset: 0,
            num_obj_info_blocks: 1,
            block_update_info: vec![BlockUpdateInfo {
                block_offset_factor_bits: 0,
                ramp_duration_code: 2,
                ramp_duration: 1536,
            }],
        };
        element.object_data = objects;
        p.object_element = Some(element);
        p
    }

    #[test]
    fn maps_bed_and_dynamic_objects_with_adm_axes() {
        // bed: L, R (std bed 0 = first 2 entries of STD_BED_LIST[0] = L,R)
        let mut bed_l = object_block(true, 0);
        bed_l.object_render_info = render_info(0.5, 0.5, 0.0);
        let mut bed_r = object_block(true, 0);
        bed_r.object_render_info = render_info(0.5, 0.5, 0.0);
        // dynamic object: DAMF right/front/up = (0.9, 0.2, 0.8)
        let mut dyn_obj = object_block(false, -6);
        dyn_obj.object_render_info = render_info(0.9, 0.2, 0.8);
        let p = payload(vec![vec![bed_l], vec![bed_r], vec![dyn_obj]], 0x1);
        let meta = from_oamd(&p).unwrap();
        assert_eq!(meta.object_count, 3);
        assert_eq!(meta.bed_speakers, vec![ChannelId::FL, ChannelId::FR]);
        assert_eq!(meta.dynamic_objects, 1);
        assert_eq!(meta.ramp_samples, 1536);
        assert!(meta.objects[0].in_bed);
        assert_eq!(meta.objects[0].bed_channel, Some(ChannelId::FL));
        let dyn_attr = meta.dynamic().next().unwrap();
        assert_eq!(dyn_attr.id, 0);
        assert_eq!(dyn_attr.gain_db, -6);
        let pos = dyn_attr.position.unwrap();
        // DAMF (0.9,0.2,0.8): x'=0.8, y'=0.6, z'=0.8 -> ADM [0.8, 0.8, -0.6]
        assert!((pos[0] - 0.8).abs() < 1e-9);
        assert!((pos[1] - 0.8).abs() < 1e-9);
        assert!((pos[2] + 0.6).abs() < 1e-9);
        assert_eq!(dyn_attr.size, [0.1, 0.2, 0.3]);
    }

    #[test]
    fn gain_codes_convert_to_linear() {
        assert!((gain_db_to_linear(0) - 1.0).abs() < 1e-6);
        assert!((gain_db_to_linear(-6) - 0.501_187).abs() < 1e-4);
        assert!((gain_db_to_linear(6) - 1.995_262).abs() < 1e-4);
        assert_eq!(gain_db_to_linear(-128), 0.0);
    }

    #[test]
    fn missing_object_data_is_inactive_object() {
        let mut p = ObjectAudioMetadataPayload::default();
        p.object_count = 3;
        p.program_assignment.bed_assignment = vec![BedAssignment::from_std(0x1)]; // L,R bed
        p.object_element = Some(ObjectElement::default());
        let meta = from_oamd(&p).unwrap();
        assert_eq!(meta.bed_speakers, vec![ChannelId::FL, ChannelId::FR]);
        assert_eq!(meta.dynamic_objects, 1);
        assert_eq!(meta.objects[2].gain_db, i8::MIN);
        assert!(meta.objects[2].position.is_none());
    }
}
