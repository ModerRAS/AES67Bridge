//! Offline render benchmark/probe: TrueHD file -> ObjectPipeline -> 7.1.4/5.1.2/9.1.6 blocks.
//!
//! Usage: render-probe <file> [target=7.1.4] [--blocks N]

use dolby_pipeline::{BlockPipeline, LayoutSpec, ObjectPipeline};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: render-probe <file> [target layout: 7.1.4|5.1.2|9.1.6|7.1.2|5.1.4]");
        std::process::exit(2);
    }
    let target = args.get(2).map(String::as_str).unwrap_or("7.1.4");
    if LayoutSpec::known(target).is_none() {
        eprintln!("unknown target layout {target:?}");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1])?;
    let mut pipeline = ObjectPipeline::new(target)?;
    let started = Instant::now();
    let blocks = pipeline.push_bits(&bytes)?;
    let elapsed = started.elapsed();
    let stats = pipeline.decode_stats();
    println!(
        "target={target} codec={} blocks={} wall={:.3} ms decode_aus={} decode_errs={}",
        pipeline.es_codec().as_str(),
        blocks.len(),
        elapsed.as_secs_f64() * 1e3,
        stats.aus_decoded,
        stats.decode_errors
    );
    for block in blocks.iter().take(3) {
        let energy: Vec<f64> = block
            .channels
            .iter()
            .map(|c| c.iter().map(|&s| s as f64 * s as f64).sum::<f64>())
            .collect();
        let labels: Vec<&str> = block.actual.channels().iter().map(|c| c.canonical()).collect();
        let energy_str: Vec<String> = energy
            .iter()
            .zip(labels.iter())
            .filter(|(e, _)| **e > 1e-9)
            .map(|(e, label)| format!("{label}:{:.4}", e))
            .collect();
        println!(
            "block seq={} pos={} target={} actual={} objects={} energy[{}]",
            block.seq,
            block.sample_pos,
            block.target.id(),
            block.actual.id(),
            block.object_count,
            energy_str.join(" ")
        );
    }
    if let Some(last) = blocks.last() {
        println!(
            "last block: seq={} pos={} ({} samples served)",
            last.seq,
            last.sample_pos,
            last.sample_pos + last.frames as u64
        );
    }

    // Whole-run energy summary (not just the first blocks): catches silent or partially
    // rendered streams without dumping every block.
    let mut blocks_with_energy = 0usize;
    let mut first_loud: Option<u64> = None;
    let mut sums: Vec<f64> = Vec::new();
    let mut peaks: Vec<f32> = Vec::new();
    for block in &blocks {
        if block.channels.len() > sums.len() {
            sums.resize(block.channels.len(), 0.0);
            peaks.resize(block.channels.len(), 0.0);
        }
        let mut e = 0.0f64;
        for (c, channel) in block.channels.iter().enumerate() {
            let mut ch_sum = 0.0f64;
            let mut peak = 0.0f32;
            for &s in channel {
                ch_sum += s as f64 * s as f64;
                peak = peak.max(s.abs());
            }
            sums[c] += ch_sum;
            peaks[c] = peaks[c].max(peak);
            e += ch_sum;
        }
        if e > 1e-9 {
            blocks_with_energy += 1;
            if first_loud.is_none() {
                first_loud = Some(block.seq);
            }
        }
    }
    let frames_total: usize = blocks.iter().map(|b| b.frames).sum();
    let ch = sums.len();
    let rms: Vec<f64> = sums.iter().map(|s| (s / frames_total as f64).sqrt()).collect();
    println!(
        "summary: blocks={} blocks_with_energy={} first_loud_block={:?} frames={} channels={}",
        blocks.len(),
        blocks_with_energy,
        first_loud,
        frames_total,
        ch
    );
    println!(
        "summary_rms[{}]",
        rms.iter().enumerate().map(|(i, r)| format!("{i}:{r:.6}")).collect::<Vec<_>>().join(" ")
    );
    println!(
        "summary_peak[{}]",
        peaks.iter().enumerate().map(|(i, p)| format!("{i}:{p:.6}")).collect::<Vec<_>>().join(" ")
    );
    let _ = BlockPipeline::new(
        &LayoutSpec::known("7.1.4").unwrap(),
        &LayoutSpec::roar("7.1.4").unwrap(),
        480,
    );
    Ok(())
}
