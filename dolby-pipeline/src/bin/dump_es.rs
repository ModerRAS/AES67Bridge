//! live.bits -> ES dump + framing diagnostics.
//!
//! Phase-0 conclusion: live.bits bytes are forwarded unchanged; the 4-byte prefix of every TrueHD
//! access unit is part of the code stream (AU header), not a burst wrapper. This tool makes that
//! auditable: it reports the codec, first AU header, AU count and 40-sample counter continuity,
//! and writes the bytes unchanged.

use dolby_pipeline::bits::{detect_codec, AuIter, EsNormalizer, TrueHdAuHeader};
use dolby_pipeline::Codec;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: dump-es <live.bits | es file> [out.es]");
        std::process::exit(2);
    }
    let input = std::fs::read(&args[1])?;
    let mut normalizer = EsNormalizer::new();
    let forwarded = normalizer.push(&input);
    let codec = detect_codec(&input);
    println!("input {} bytes, codec={}", input.len(), codec.as_str());
    println!(
        "normalizer codec={} bytes_in={} first_major_sync_offset={:?}",
        normalizer.codec().as_str(),
        normalizer.bytes_in(),
        normalizer.first_major_sync_offset()
    );
    match codec {
        Codec::TrueHd => {
            let first = TrueHdAuHeader::parse(&input)?;
            println!(
                "first AU header: len_bytes={} flags=0x{:x} sample_counter={}",
                first.len_bytes, first.flags, first.sample_counter
            );
            let mut aus = 0usize;
            let mut last_counter: Option<u16> = None;
            let mut discontinuities = 0usize;
            for item in AuIter::new(&input) {
                let (_au, header) = item?;
                if let Some(prev) = last_counter {
                    if header.sample_counter != prev.wrapping_add(40) {
                        discontinuities += 1;
                    }
                }
                last_counter = Some(header.sample_counter);
                aus += 1;
            }
            println!("TrueHD AUs parsed: {aus}, counter discontinuities: {discontinuities}");
        }
        _ => eprintln!("note: this tool only inspects TrueHD AU framing; bytes stay unchanged"),
    }
    if let Some(path) = args.get(2) {
        let mut file = std::fs::File::create(path)?;
        file.write_all(forwarded)?;
        println!("wrote {} bytes to {path} (unchanged pass-through)", forwarded.len());
    }
    Ok(())
}
