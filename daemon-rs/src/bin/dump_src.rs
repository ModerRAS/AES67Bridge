//! Debug helper: decode a raw PCM file through the daemon input path
//! (sample format decode + resampler) and write 24-bit big-endian payload bytes.
use std::io::Write;
use aes67d::pcm::SampleFmt;
use aes67d::resample::{PolyphaseResampler, Resampler, RubatoResampler};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).expect("input raw");
    let out = args.get(2).expect("output raw");
    let channels: usize = args.get(3).map(|v| v.parse().unwrap()).unwrap_or(6);
    let rate: u32 = args.get(4).map(|v| v.parse().unwrap()).unwrap_or(44100);
    let resampler_name = args.get(5).cloned().unwrap_or_else(|| "c-poly".into());
    let data = std::fs::read(input).unwrap();
    let fmt = SampleFmt::S16;
    let frame_bytes = fmt.bytes_per_sample() * channels;
    let mut frame = [0_i32; aes67d::MAX_CH];
    let mut resampler: Box<dyn Resampler> = if resampler_name == "rubato" {
        Box::new(RubatoResampler::new(channels).unwrap())
    } else {
        Box::new(PolyphaseResampler::new())
    };
    let mut payload = Vec::new();
    let mut emit = |f: &[i32; aes67d::MAX_CH]| {
        for c in 0..channels {
            let v = f[c] & 0xFFFFFF;
            payload.push(((v >> 16) & 0xFF) as u8);
            payload.push(((v >> 8) & 0xFF) as u8);
            payload.push((v & 0xFF) as u8);
        }
    };
    for chunk in data.chunks_exact(frame_bytes) {
        fmt.decode_frame(chunk, channels, &mut frame);
        if rate == 48000 {
            emit(&frame);
        } else {
            resampler.push(&frame, channels, &mut emit);
        }
    }
    resampler.flush(channels, &mut emit);
    std::fs::File::create(out).unwrap().write_all(&payload).unwrap();
    eprintln!("frames_in={} frames_out={} bytes={}", resampler.rs_in(), resampler.rs_out(), payload.len());
}
