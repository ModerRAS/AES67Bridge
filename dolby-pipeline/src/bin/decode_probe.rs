//! Offline TrueHD decode probe: ES/bits -> decode statistics (and optional 7.1.4 f32 WAV).
//!
//! Usage: decode-probe <file> [--wav out.wav] [--blocks N]

use dolby_pipeline::decode::{BLOCK_FRAMES, I32_PCM_FULL_SCALE};
use dolby_pipeline::{ObjectPipeline, TrueHdDecoder};
use std::io::Write;

fn write_wav_f32(path: &str, channels: usize, rate: u32, interleaved: &[f32]) -> std::io::Result<()> {
    let frames = interleaved.len() / channels;
    let data_bytes = (interleaved.len() * 4) as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&(channels as u16).to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * channels as u32 * 4).to_le_bytes())?; // byte rate
    f.write_all(&((channels * 4) as u16).to_le_bytes())?; // block align
    f.write_all(&32u16.to_le_bytes())?; // bits
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for sample in interleaved {
        f.write_all(&sample.to_le_bytes())?;
    }
    println!("wrote {path}: {frames} frames x {channels} ch @ {rate} Hz f32");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: decode-probe <es|bits file> [--wav out.wav] [--blocks N]");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1])?;
    let wav = args.iter().position(|a| a == "--wav").and_then(|i| args.get(i + 1)).cloned();
    let max_blocks: Option<usize> = args
        .iter()
        .position(|a| a == "--blocks")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());

    // Full decode statistics.
    let mut decoder = TrueHdDecoder::new();
    let aus = decoder.push_es(&bytes);
    let stats = decoder.stats();
    println!(
        "decode: es_bytes={} aus={} major_syncs={} skipped_pre_sync={} extract_errs={} parse_errs={} decode_errs={} oamd_payloads={}",
        stats.es_bytes,
        stats.aus_decoded,
        stats.major_syncs,
        stats.aus_skipped_pre_sync,
        stats.extract_errors,
        stats.parse_errors,
        stats.decode_errors,
        stats.oamd_payloads
    );
    let dialnorm = decoder.dialnorm();
    println!(
        "dialnorm: twoch={} sixch={} eightch={} sixteench={} lfe_present={} dyn_object_only={}",
        dialnorm.twoch,
        dialnorm.sixch,
        dialnorm.eightch,
        dialnorm.sixteench,
        dialnorm.lfe_present,
        dialnorm.dyn_object_only
    );
    if let Some(au) = aus.first() {
        println!(
            "first AU: samples={} p2_ch={} p3_ch={} objects={}",
            au.sample_length,
            au.p2_channels,
            au.p3_channels,
            au.objects.len()
        );
    }
    let object_aus = aus.iter().filter(|au| !au.objects.is_empty()).count();
    let max_objects = aus.iter().map(|au| au.objects.len()).max().unwrap_or(0);
    println!("AUs with objects: {object_aus}, max dynamic objects per AU: {max_objects}");

    if let Some(wav_path) = wav {
        // Optional end-to-end render to a 7.1.4 f32 WAV via the product pipeline.
        let mut pipeline = ObjectPipeline::new("7.1.4")?;
        let blocks = pipeline.push_bits(&bytes)?;
        let frames_total: usize = blocks.iter().map(|b| b.frames).sum();
        println!(
            "rendered {} blocks ({} frames) target={} actual={} objects={}",
            blocks.len(),
            frames_total,
            pipeline.target_layout().id(),
            blocks.first().map(|b| b.actual.id()).unwrap_or("?"),
            blocks.first().map(|b| b.object_count).unwrap_or(0)
        );
        let channels = blocks.first().map(|b| b.channels.len()).unwrap_or(0);
        let limit = max_blocks.unwrap_or(usize::MAX).min(blocks.len());
        let mut interleaved = Vec::with_capacity(limit * BLOCK_FRAMES * channels);
        for block in blocks.iter().take(limit) {
            for frame in 0..block.frames {
                for channel in 0..channels {
                    interleaved.push(block.channels[channel][frame]);
                }
            }
        }
        write_wav_f32(&wav_path, channels, 48000, &interleaved)?;
        let _ = I32_PCM_FULL_SCALE;
    }
    Ok(())
}
