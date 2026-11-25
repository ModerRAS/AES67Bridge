//! Offline verification against the reference TrueHD dumps in this workspace.
//!
//! Tests read \`artifacts/aes67/haltee-v5/evidence/\` and the ffmpeg reference decode under
//! \`scratch/aes67-truehd-2026-11-24/\` (both outside the crate), and skip cleanly when those
//! development artifacts are absent (e.g. a packaged checkout).

use dolby_pipeline::decode::{DecodeMode, TrueHdDecoder, AU_SAMPLES};
use dolby_pipeline::layout::ChannelId;
use dolby_pipeline::ObjectPipeline;

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn evidence(name: &str) -> Option<Vec<u8>> {
    let path = workspace_root().join("artifacts/aes67/haltee-v5/evidence").join(name);
    std::fs::read(&path).ok()
}

fn read_wav_pcm24(path: &std::path::Path) -> Option<(usize, usize, Vec<i32>)> {
    let bytes = std::fs::read(path).ok()?;
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut channels = 0usize;
    let mut bits = 0usize;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = bytes.get(pos + 8..pos + 8 + size)?;
        match id {
            b"fmt " => {
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap()) as usize;
                bits = u16::from_le_bytes(body[14..16].try_into().unwrap()) as usize;
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos += 8 + size + (size & 1);
    }
    let data = data?;
    if bits != 24 || channels == 0 {
        return None;
    }
    let mut samples = Vec::with_capacity(data.len() / 3);
    for chunk in data.chunks_exact(3) {
        let value = i32::from_le_bytes([
            chunk[0],
            chunk[1],
            chunk[2],
            if chunk[2] & 0x80 != 0 { 0xFF } else { 0x00 },
        ]);
        samples.push(value);
    }
    Some((channels, bits, samples))
}

/// ffmpeg's WAVE 7.1 channel order is FL,FR,FC,LFE,BL,BR,SL,SR; our bed slots use the ROAR
/// (-714) channel identities. This is the mapping from our bed slot to ffmpeg WAVE channel index.
const FFMPEG_71_ORDER: [ChannelId; 8] = [
    ChannelId::FL,
    ChannelId::FR,
    ChannelId::FC,
    ChannelId::LFE,
    ChannelId::LRS,
    ChannelId::RRS,
    ChannelId::LS,
    ChannelId::RS,
];

fn max_bed_diff_vs_ffmpeg(aus: &[dolby_pipeline::DecodedAu], reference: &[i32], channels: usize) -> (usize, i64) {
    let mut count = 0usize;
    let mut max_diff = 0i64;
    for (frame_index, au) in aus.iter().enumerate() {
        let base = frame_index * AU_SAMPLES;
        for (wave_channel, channel) in FFMPEG_71_ORDER.iter().enumerate() {
            let bed_slot = dolby_pipeline::BED_LAYOUT_916.iter().position(|c| c == channel).unwrap();
            for sample in 0..au.sample_length {
                let ours = (au.bed[bed_slot][sample] * (1u32 << 23) as f32).round() as i32;
                let theirs = reference[(base + sample) * channels + wave_channel];
                max_diff = max_diff.max((ours as i64 - theirs as i64).abs());
                count += 1;
            }
        }
    }
    (count, max_diff)
}

#[test]
fn bits19_decodes_to_8ch_bed_2428_aus() {
    let Some(es) = evidence("bits_19_fmt0e000000.bin") else {
        eprintln!("skip: bits_19 evidence missing");
        return;
    };
    let mut decoder = TrueHdDecoder::new();
    let aus = decoder.push_es(&es);
    let stats = decoder.stats();
    assert_eq!(stats.es_bytes, 114_688);
    assert_eq!(stats.aus_decoded, 2_428);
    assert_eq!(stats.parse_errors, 0);
    assert_eq!(stats.decode_errors, 0);
    assert_eq!(stats.major_syncs, 21);
    assert_eq!(aus.len(), 2_428);
    let samples: usize = aus.iter().map(|au| au.sample_length).sum();
    assert_eq!(samples, 2_428 * AU_SAMPLES);
    assert_eq!(samples, 97_120);
    // OAMD: 65 payloads; 12 elements = 1 bed (LFE) + 11 dynamic objects.
    assert_eq!(stats.oamd_payloads, 65);
    let metadata = aus.iter().find_map(|au| au.metadata.as_ref()).expect("metadata");
    assert_eq!(metadata.object_count, 12);
    assert_eq!(metadata.dynamic_objects, 11);
    assert_eq!(metadata.bed_speakers, vec![ChannelId::LFE]);
    // Attributes are carried forward, so nearly every AU after the first OAMD has 11 object PCM.
    let with_objects = aus.iter().filter(|au| au.objects.len() == 11).count();
    assert!(with_objects > 2_000, "objects carried forward: {with_objects}");
}

#[test]
fn bits19_bed_only_is_bit_exact_with_ffmpeg_reference() {
    let Some(es) = evidence("bits_19_fmt0e000000.bin") else {
        eprintln!("skip: bits_19 evidence missing");
        return;
    };
    let wav = workspace_root().join("scratch/aes67-truehd-2026-11-24/ffmpeg_bed.wav");
    let Some((channels, _bits, reference)) = read_wav_pcm24(&wav) else {
        eprintln!("skip: ffmpeg reference wav missing {}", wav.display());
        return;
    };
    assert_eq!(channels, 8);
    let mut decoder = TrueHdDecoder::bed_only();
    assert_eq!(decoder.mode(), DecodeMode::BedOnly);
    let aus = decoder.push_es(&es);
    assert_eq!(decoder.stats().decode_errors, 0);
    let frames: usize = aus.iter().map(|au| au.sample_length).sum();
    assert_eq!(frames, 97_120);
    let (count, max_diff) = max_bed_diff_vs_ffmpeg(&aus, &reference, channels);
    assert_eq!(count, frames * 8);
    assert_eq!(max_diff, 0, "bed PCM differs from ffmpeg by {max_diff} (24-bit LSB)");
    eprintln!("bits_19 bed-only: {count} samples (97,120 x 8ch) bit-exact with ffmpeg");
}

#[test]
fn full_reference_bed_only_is_bit_exact_with_ffmpeg() {
    let ref_path = workspace_root().join("scratch/aes67-truehd-2026-11-24/ref_truehd.thd");
    let wav = workspace_root().join("scratch/aes67-truehd-2026-11-24/ffmpeg_bed.wav");
    let Ok(es) = std::fs::read(&ref_path) else {
        eprintln!("skip: {} missing", ref_path.display());
        return;
    };
    let Some((channels, _bits, reference)) = read_wav_pcm24(&wav) else {
        eprintln!("skip: {} missing", wav.display());
        return;
    };
    let mut decoder = TrueHdDecoder::bed_only();
    let aus = decoder.push_es(&es);
    let frames: usize = aus.iter().map(|au| au.sample_length).sum();
    assert_eq!(frames, 576_680);
    let (count, max_diff) = max_bed_diff_vs_ffmpeg(&aus, &reference, channels);
    assert_eq!(count, frames * 8);
    assert_eq!(max_diff, 0, "full-reference bed differs from ffmpeg by {max_diff}");
    eprintln!("full reference: {} samples bit-exact with ffmpeg_bed.wav", count);
}

#[test]
fn bits20_object_pipeline_714_renders_atmos_objects() {
    let Some(es) = evidence("bits_20_fmt0e000000.bin") else {
        eprintln!("skip: bits_20 evidence missing");
        return;
    };
    let mut pipeline = ObjectPipeline::new("7.1.4").expect("pipeline");
    let blocks = pipeline.push_bits(&es).expect("decode+render");
    assert_eq!(blocks.len(), 548);
    assert_eq!(pipeline.decode_stats().aus_decoded, 6_580);
    assert_eq!(pipeline.decode_stats().decode_errors, 0);
    let mut total_energy = 0.0f64;
    for (index, block) in blocks.iter().enumerate() {
        assert_eq!(block.frames, 480);
        assert_eq!(block.sample_pos, index as u64 * 480);
        assert_eq!(block.actual.id(), "7.1.4");
        assert_eq!(block.object_count, 11, "block {index}");
        assert_eq!(block.channels.len(), 12);
        assert!(block.channels.iter().all(|c| c.iter().all(|s| s.is_finite())));
        total_energy += block
            .channels
            .iter()
            .map(|c| c.iter().map(|&s| s as f64 * s as f64).sum::<f64>())
            .sum::<f64>();
    }
    assert!(total_energy > 100.0, "rendered output is (nearly) silent: {total_energy}");
    eprintln!("bits_20: 548 blocks x 480 frames, 11 objects, total energy {total_energy:.1}");
}

#[test]
fn bits20_object_pipeline_512_renders_and_publishes_intersection() {
    let Some(es) = evidence("bits_20_fmt0e000000.bin") else {
        eprintln!("skip: bits_20 evidence missing");
        return;
    };
    let mut pipeline = ObjectPipeline::new("5.1.2").expect("pipeline");
    let blocks = pipeline.push_bits(&es).expect("decode+render");
    assert!(!blocks.is_empty());
    let block = &blocks[0];
    assert_eq!(block.actual.id(), "5.1.2");
    assert_eq!(block.channels.len(), 8);
    assert_eq!(block.channel_labels(), "L,R,C,LFE,Ls,Rs,Tfl,Tfr");
}

#[test]
fn device_5_1_truehd_dump2_decodes_with_presentation_fallback() {
    // Real device capture (2026-11-25): standard 48 kHz Dolby TrueHD 5.1, 2 substreams,
    // presentations [1,3,3,0] (no Atmos presentation 3), format_info 0x0007800F. The first AU
    // starts at byte 384 (FIFO carried the tail of the previous stream), then 342 major syncs.
    let path = workspace_root().join("scratch/aes67-rust-daemon-2026-11-24/e2e-hal-real/dump2.bits");
    let Ok(es) = std::fs::read(&path) else {
        eprintln!("skip: {} missing", path.display());
        return;
    };
    assert_eq!(es.len(), 591_488);
    let mut decoder = TrueHdDecoder::new();
    let aus = decoder.push_es(&es);
    let stats = decoder.stats();
    assert_eq!(stats.aus_decoded, 5_467, "stats: {stats:?}");
    assert_eq!(stats.decode_errors, 0, "stats: {stats:?}");
    assert_eq!(stats.major_syncs, 342, "stats: {stats:?}");
    assert_eq!(stats.oamd_payloads, 0, "5.1 TrueHD has no OAMD");
    // One AU mid-capture is corrupt (filter order out of range): the extractor/parser reports it
    // and decoding continues. That is a capture/stream splice, not a format the decoder cannot read.
    assert_eq!(stats.parse_errors, 1, "stats: {stats:?}");
    let samples: usize = aus.iter().map(|au| au.sample_length).sum();
    assert_eq!(samples, 218_680);
    assert!(aus.iter().all(|au| au.objects.is_empty()), "5.1 stream must not produce objects");
    assert!(aus.iter().all(|au| au.p2_channels == 6), "bed presentation is 5.1");
    assert_eq!(aus[0].bed.len(), dolby_pipeline::BED_CHANNELS);

    // End-to-end: target 7.1.4 receives 5.1 content; published layout is the strict intersection.
    let mut pipeline = ObjectPipeline::new("7.1.4").expect("pipeline");
    let blocks = pipeline.push_bits(&es).expect("decode+render");
    assert_eq!(blocks.len(), 218_680 / 480);
    let block = &blocks[0];
    assert_eq!(block.actual.id(), "5.1");
    assert_eq!(block.channel_labels(), "L,R,C,LFE,Ls,Rs");
    assert_eq!(block.object_count, 0);
    let energy: f64 = blocks
        .iter()
        .map(|b| b.channels.iter().map(|c| c.iter().map(|&s| s as f64 * s as f64).sum::<f64>()).sum::<f64>())
        .sum();
    assert!(energy > 10.0, "device 5.1 TrueHD rendered (nearly) silence (energy {energy})");
    let first = &blocks[0].channels;
    assert!(first.iter().all(|c| c.iter().all(|s| s.is_finite())));
}

#[test]
fn device_dump2_bed_matches_truehdd_reference_layout() {
    // tdtest/truehdd decode of the same dump: 5,467 AUs, 6ch, labels L,R,C,LFE,Ls,Rs.
    let path = workspace_root().join("scratch/aes67-rust-daemon-2026-11-24/e2e-hal-real/dump2.bits");
    let Ok(es) = std::fs::read(&path) else {
        eprintln!("skip: {} missing", path.display());
        return;
    };
    let mut decoder = TrueHdDecoder::bed_only();
    let aus = decoder.push_es(&es);
    assert_eq!(decoder.stats().aus_decoded, 5_467, "stats: {:?}", decoder.stats());
    assert_eq!(decoder.stats().decode_errors, 0, "stats: {:?}", decoder.stats());
    let total: usize = aus.iter().map(|au| au.sample_length).sum();
    assert_eq!(total, 218_680);
    assert!(aus.iter().all(|au| au.p2_channels == 6));
}

#[test]
fn mkv_5_1_truehd_variant_decodes_12000_aus() {
    // TrueHD 5.1 track demuxed from scratch/haltee.../"haltee-hevc-truehd-7.1.mkv" (legal,
    // stream-verified 5.1 at 48 kHz, format_info=0x0007800F). It has no independent Atmos
    // presentation: masks [1,3,3,0], p1 = 6ch Independent, p2 = CopyOf(1), p3 Invalid.
    let path = workspace_root().join("scratch/truehd-variant-2026-11-25/haltee_5_1.thd");
    let Ok(es) = std::fs::read(&path) else {
        eprintln!("skip: {} missing", path.display());
        return;
    };
    let mut decoder = TrueHdDecoder::new();
    let aus = decoder.push_es(&es);
    let stats = decoder.stats();
    assert_eq!(stats.aus_decoded, 12_000, "stats: {stats:?}");
    assert_eq!(stats.major_syncs, 750, "stats: {stats:?}");
    assert_eq!(stats.decode_errors, 0, "stats: {stats:?}");
    assert_eq!(stats.oamd_payloads, 0, "5.1 TrueHD must not carry OAMD");
    assert!(aus.iter().all(|au| au.p2_channels == 6));
    assert!(aus.iter().all(|au| au.objects.is_empty()));
    let samples: usize = aus.iter().map(|au| au.sample_length).sum();
    assert_eq!(samples, 480_000);

    let mut pipeline = ObjectPipeline::new("7.1.4").expect("pipeline");
    let blocks = pipeline.push_bits(&es).expect("decode+render");
    assert_eq!(blocks.len(), 1_000);
    assert!(blocks.iter().all(|b| b.actual.id() == "5.1"));
    let energy: f64 = blocks
        .iter()
        .map(|b| b.channels.iter().map(|c| c.iter().map(|&s| s as f64 * s as f64).sum::<f64>()).sum::<f64>())
        .sum();
    assert!(energy > 10.0, "5.1 variant rendered (nearly) silence (energy {energy})");
}

#[test]
fn device_preroll_segment_is_coded_silence_not_a_render_bug() {
    // Continuous-capture preroll: 49,152 B of a 7.1 Atmos stream taken before the first audible
    // sample. It decodes to 1,039 AUs / 86 blocks and the PCM is digital silence (both truehdd and
    // ffmpeg decode the same bytes to zero), so "all-zero energy" here is content, not a render bug.
    let path = workspace_root()
        .join("scratch/aes67-rust-daemon-2026-11-24/hal-dump-watch/dump39/dump.bits");
    let Ok(es) = std::fs::read(&path) else {
        eprintln!("skip: {} missing", path.display());
        return;
    };
    let mut pipeline = ObjectPipeline::new("7.1.4").expect("pipeline");
    let blocks = pipeline.push_bits(&es).expect("decode+render");
    assert_eq!(blocks.len(), 86);
    assert_eq!(pipeline.decode_stats().aus_decoded, 1_039);
    assert_eq!(pipeline.decode_stats().decode_errors, 0);
    let energy: f64 = blocks
        .iter()
        .map(|b| b.channels.iter().map(|c| c.iter().map(|&s| s as f64 * s as f64).sum::<f64>()).sum::<f64>())
        .sum();
    assert!(energy < 1e-6, "preroll segment unexpectedly contains audio (energy {energy})");
}

#[test]
fn presentation_rearm_handles_layout_change_mid_stream() {
    // The device FIFO can concatenate two different TrueHD streams (player switched title/track);
    // dump2.bits itself starts with 384 bytes of the previous stream. The decoder must re-lock the
    // parser and presentation mask at the new stream's major sync instead of stalling on parse
    // errors. Stream A: legal 5.1 (format 0x0007800F), stream B: 7.1 + 11 objects (0x0017804F).
    let a = workspace_root().join("scratch/truehd-variant-2026-11-25/haltee_5_1.thd");
    let b = workspace_root().join("artifacts/aes67/haltee-v5/evidence/bits_19_fmt0e000000.bin");
    let (Ok(mut a), Ok(b)) = (std::fs::read(&a), std::fs::read(&b)) else {
        eprintln!("skip: switch fixtures missing");
        return;
    };
    a.extend_from_slice(&b);
    let mut decoder = TrueHdDecoder::new();
    let aus = decoder.push_es(&a);
    let stats = decoder.stats();
    assert_eq!(stats.decode_errors, 0, "stats: {stats:?}");
    assert_eq!(stats.parse_errors, 0, "stats: {stats:?}");
    assert!(
        (14_400..=14_430).contains(&stats.aus_decoded),
        "both segments must decode: {stats:?}"
    );
    assert!(aus[0].objects.is_empty(), "first segment is 5.1");
    assert_eq!(aus[0].p2_channels, 6);
    let object_aus = aus.iter().filter(|au| !au.objects.is_empty()).count();
    assert!((2_400..=2_430).contains(&object_aus), "object AUs: {object_aus}");
    assert!(!aus.last().unwrap().objects.is_empty(), "decoder must be back in Atmos mode");
}
