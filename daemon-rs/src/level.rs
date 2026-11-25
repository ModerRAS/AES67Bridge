//! Per-channel RMS accumulator for status.json `levels` (dBFS, -120 floor).
//! Matches the C formula: rms = sqrt(sumsq / frames), dbfs = 20*log10(rms / 2^23).

use crate::pcm::MAX_CH;

pub struct LevelMeter {
    sumsq: [f64; MAX_CH],
    frames: u64,
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self {
            sumsq: [0.0; MAX_CH],
            frames: 0,
        }
    }
}

impl LevelMeter {
    pub fn add_frame(&mut self, frame: &[i32; MAX_CH], channels: usize) {
        for c in 0..channels.min(MAX_CH) {
            let v = f64::from(frame[c]);
            self.sumsq[c] += v * v;
        }
        self.frames += 1;
    }

    /// Accumulate a planar f32 block (target layout order) produced by the ROAR renderer.
    pub fn add_planar(&mut self, block: &[Vec<f32>], frames: usize) {
        for c in 0..block.len().min(MAX_CH) {
            let channel = &block[c];
            let mut sumsq = 0.0f64;
            for &sample in channel.iter().take(frames) {
                let v = f64::from(sample) * 8_388_608.0;
                sumsq += v * v;
            }
            self.sumsq[c] += sumsq;
        }
        self.frames += frames as u64;
    }

    pub fn add_group(&mut self, frames: &[[i32; MAX_CH]], channels: usize) {
        for frame in frames {
            for c in 0..channels.min(MAX_CH) {
                let v = f64::from(frame[c]);
                self.sumsq[c] += v * v;
            }
        }
        self.frames += frames.len() as u64;
    }

    pub fn dbfs(&self) -> [f64; MAX_CH] {
        let mut out = [-120.0; MAX_CH];
        if self.frames > 0 {
            for c in 0..MAX_CH {
                if self.sumsq[c] > 0.0 {
                    let rms = (self.sumsq[c] / self.frames as f64).sqrt();
                    let dbfs = 20.0 * (rms / 8_388_608.0).log10();
                    out[c] = dbfs.clamp(-120.0, 0.0);
                }
            }
        }
        out
    }

    pub fn reset(&mut self) {
        self.sumsq = [0.0; MAX_CH];
        self.frames = 0;
    }
}
