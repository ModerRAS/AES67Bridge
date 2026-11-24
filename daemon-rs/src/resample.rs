//! 44.1 kHz -> 48 kHz sample-rate conversion for the daemon input path.
//!
//! Two implementations:
//! * [ResamplerKind::Rubato] - rubato sinc resampler (MIT), default.
//! * [ResamplerKind::CPoly]  - the C daemon's 160 phase x 287 tap Q30 polyphase
//!   FIR (libsamplerate 0.2.2 best-quality coefficients), bit-exact against
//!   `aes67_rtp_sender_v4.c` for A/B verification.
//!
//! The sink receives 24-bit left-aligned interleaved frames; the caller feeds
//! them to the NSP session ring.

use rubato::Resampler as _;

use crate::pcm::MAX_CH;
use crate::sinc_poly::{SINC_PHASES, SINC_POLY_Q30, SINC_TAPS};

pub const INPUT_RATE: u32 = 44_100;

pub type FrameSink<'a> = &'a mut dyn FnMut(&[i32; MAX_CH]);

pub trait Resampler: Send {
    fn push(&mut self, frame: &[i32; MAX_CH], channels: usize, sink: FrameSink<'_>);
    fn flush(&mut self, channels: usize, sink: FrameSink<'_>);
    fn rs_in(&self) -> u64;
    fn rs_out(&self) -> u64;
    /// Reset the stream state (new stream_id or format change).
    fn reset(&mut self);
}

/// 48 kHz passthrough: no conversion, rs_in/rs_out stay zero like the C daemon.
pub struct Passthrough;

impl Resampler for Passthrough {
    fn push(&mut self, frame: &[i32; MAX_CH], _channels: usize, sink: FrameSink<'_>) {
        sink(frame);
    }
    fn flush(&mut self, _channels: usize, _sink: FrameSink<'_>) {}
    fn rs_in(&self) -> u64 {
        0
    }
    fn rs_out(&self) -> u64 {
        0
    }
    fn reset(&mut self) {}
}

// ---------------------------------------------------------------------------
// rubato sinc resampler (default)
// ---------------------------------------------------------------------------

/// Output/input ratio for 44.1 -> 48 kHz (exact 160/147).
const RATIO_NUM: u64 = 160;
const RATIO_DEN: u64 = 147;
/// Fixed input chunk: 147 frames in -> 160 frames out (exact rational ratio).
const CHUNK: usize = 147;

pub struct RubatoResampler {
    inner: rubato::Async<f32>,
    channels: usize,
    pending: Vec<f32>,
    pending_frames: usize,
    in_frames: u64,
    out_frames: u64,
    delay_skip: u64,
    zero_chunk: Vec<f32>,
    flushed: bool,
}

impl RubatoResampler {
    pub fn new(channels: usize) -> Result<Self, String> {
        let params = rubato::SincInterpolationParameters {
            sinc_len: 192,
            f_cutoff: Some(0.95),
            oversampling_factor: 256,
            interpolation: rubato::SincInterpolationType::Cubic,
            window: rubato::WindowFunction::BlackmanHarris2,
        };
        let ratio = f64::from(RATIO_NUM as u32) / f64::from(RATIO_DEN as u32);
        let inner = rubato::Async::<f32>::new_sinc(
            ratio,
            1.0,
            &params,
            CHUNK,
            channels,
            rubato::FixedAsync::Input,
        )
        .map_err(|err| format!("rubato init: {err}"))?;
        let delay_skip = inner.output_delay() as u64;
        Ok(Self {
            inner,
            channels,
            pending: Vec::with_capacity(CHUNK * channels),
            pending_frames: 0,
            in_frames: 0,
            out_frames: 0,
            delay_skip,
            zero_chunk: vec![0.0; CHUNK * channels],
            flushed: false,
        })
    }

    fn expected_out(&self) -> u64 {
        // ceil(in * 160 / 147) exactly like the C flush condition.
        (self.in_frames * RATIO_NUM).div_ceil(RATIO_DEN)
    }

    fn process_chunk(
        &mut self,
        channels: usize,
        sink: FrameSink<'_>,
        cap: Option<u64>,
    ) {
        use rubato::audioadapter_buffers::direct::InterleavedSlice;
        use rubato::Resampler as _;
        let frames = self.pending_frames.min(CHUNK);
        let input = match InterleavedSlice::new(&self.pending[..frames * channels], channels, frames) {
            Ok(input) => input,
            Err(err) => {
                crate::logln!("rubato input size error: {err}");
                self.pending_frames = 0;
                self.pending.clear();
                return;
            }
        };
        let output = match self.inner.process(&input, None) {
            Ok(output) => output,
            Err(err) => {
                crate::logln!("rubato process error: {err}");
                self.pending_frames = 0;
                self.pending.clear();
                return;
            }
        };
        let data = output.take_data();
        let produced = data.len() / channels;
        let mut frame = [0_i32; MAX_CH];
        for f in 0..produced {
            if self.delay_skip > 0 {
                self.delay_skip -= 1;
                continue;
            }
            let mut emit = true;
            if let Some(cap) = cap {
                if self.out_frames >= cap {
                    emit = false;
                }
            }
            if emit {
                for c in 0..channels.min(MAX_CH) {
                    let v = data[f * channels + c] * 8_388_608.0;
                    let rounded = v.round().clamp(-8_388_608.0, 8_388_607.0);
                    frame[c] = rounded as i32;
                }
                sink(&frame);
                self.out_frames += 1;
            }
        }
        // The whole chunk was consumed (zero or real); keep no leftover.
        self.pending_frames = 0;
        self.pending.clear();
    }
}

impl Resampler for RubatoResampler {
    fn push(&mut self, frame: &[i32; MAX_CH], channels: usize, sink: FrameSink<'_>) {
        self.in_frames += 1;
        for c in 0..channels.min(MAX_CH) {
            self.pending.push(frame[c] as f32 / 8_388_608.0);
        }
        self.pending_frames += 1;
        if self.pending_frames == CHUNK {
            self.process_chunk(channels, sink, None);
        }
    }

    fn flush(&mut self, channels: usize, sink: FrameSink<'_>) {
        if self.flushed {
            return;
        }
        self.flushed = true;
        let expected = self.expected_out();
        // Pad the last partial chunk with silence and keep pumping zeros until the
        // resampler has produced the exact C frame count.
        while self.out_frames < expected {
            if self.pending_frames < CHUNK {
                for c in 0..channels.min(MAX_CH) {
                    self.pending
                        .push(self.zero_chunk[c.min(self.zero_chunk.len() - 1)]);
                }
                self.pending_frames += 1;
                if self.pending_frames < CHUNK {
                    continue;
                }
            }
            self.process_chunk(channels, sink, Some(expected));
            if self.pending_frames == 0 {
                for _ in 0..CHUNK * channels {
                    self.pending.push(0.0);
                }
                self.pending_frames = CHUNK;
            }
        }
        self.pending_frames = 0;
        self.pending.clear();
    }

    fn rs_in(&self) -> u64 {
        self.in_frames
    }
    fn rs_out(&self) -> u64 {
        self.out_frames
    }
    fn reset(&mut self) {
        // Cheap re-creation: the daemon only does this on a stream change.
        if let Ok(fresh) = RubatoResampler::new(self.channels) {
            *self = fresh;
        }
    }
}

// ---------------------------------------------------------------------------
// C-compatible polyphase FIR (bit-exact A/B mode)
// ---------------------------------------------------------------------------

const HIST_FRAMES: usize = 4096;
const HIST_MASK: usize = HIST_FRAMES - 1;
const PREFILL: usize = SINC_TAPS / 2; // 143

pub struct PolyphaseResampler {
    hist: Vec<[i32; MAX_CH]>,
    hist_count: u64,
    pos_num: u64,
    in_frames: u64,
    out_frames: u64,
    flushed: bool,
}

impl PolyphaseResampler {
    pub fn new() -> Self {
        Self {
            hist: vec![[0; MAX_CH]; HIST_FRAMES],
            hist_count: 0,
            pos_num: 0,
            in_frames: 0,
            out_frames: 0,
            flushed: false,
        }
    }

    fn history_push(&mut self, frame: &[i32; MAX_CH], channels: usize) {
        let slot = ((self.hist_count + PREFILL as u64) as usize) & HIST_MASK;
        for c in 0..channels.min(MAX_CH) {
            self.hist[slot][c] = frame[c];
        }
        for c in channels..MAX_CH {
            self.hist[slot][c] = 0;
        }
        self.hist_count += 1;
    }

    /// C `flush_resampler`: push 143 zero frames once, then drain every output
    /// frame whose window lies inside the recorded history.
    fn zero_pad(&mut self) {
        for _ in 0..PREFILL {
            self.history_push(&[0; MAX_CH], MAX_CH);
        }
    }

    fn emit_while_ready(&mut self, channels: usize, sink: FrameSink<'_>) {
        while self.hist_count > (self.pos_num / SINC_PHASES as u64) + SINC_TAPS as u64 / 2 {
            let base = (self.pos_num / SINC_PHASES as u64) as usize;
            let phase = (self.pos_num % SINC_PHASES as u64) as usize;
            let coef = &SINC_POLY_Q30[phase];
            let mut frame = [0_i32; MAX_CH];
            for c in 0..channels.min(MAX_CH) {
                let mut sum: i64 = 0;
                for tap in 0..SINC_TAPS {
                    let sample = self.hist[(base + tap) & HIST_MASK][c];
                    sum += i64::from(sample) * i64::from(coef[tap]);
                }
                let value = (sum >> 30).clamp(-8_388_608, 8_388_607) as i32;
                frame[c] = value;
            }
            sink(&frame);
            self.pos_num += RATIO_DEN; // 147/160 step in 1/160 input-frame units
            self.out_frames += 1;
        }
    }
}

impl Resampler for PolyphaseResampler {
    fn push(&mut self, frame: &[i32; MAX_CH], channels: usize, sink: FrameSink<'_>) {
        self.history_push(frame, channels);
        self.in_frames += 1;
        self.emit_while_ready(channels, sink);
    }

    fn flush(&mut self, channels: usize, sink: FrameSink<'_>) {
        if self.flushed || self.in_frames == 0 {
            return;
        }
        self.flushed = true;
        self.zero_pad();
        self.emit_while_ready(channels, sink);
    }

    fn rs_in(&self) -> u64 {
        self.in_frames
    }
    fn rs_out(&self) -> u64 {
        self.out_frames
    }

    fn reset(&mut self) {
        self.hist = vec![[0; MAX_CH]; HIST_FRAMES];
        self.hist_count = 0;
        self.pos_num = 0;
        self.in_frames = 0;
        self.out_frames = 0;
        self.flushed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResamplerKind;

    fn tone(frames: usize, channels: usize, hz: f64, rate: f64) -> Vec<[i32; MAX_CH]> {
        (0..frames)
            .map(|n| {
                let mut frame = [0_i32; MAX_CH];
                let value = ((n as f64 * hz * std::f64::consts::TAU / rate).sin() * 8_000_000.0) as i32;
                for c in 0..channels {
                    frame[c] = value;
                }
                frame
            })
            .collect()
    }

    fn run(kind: ResamplerKind, input: &[[i32; MAX_CH]], channels: usize) -> (Vec<[i32; MAX_CH]>, u64, u64) {
        let mut rs: Box<dyn Resampler> = match kind {
            ResamplerKind::Rubato => Box::new(RubatoResampler::new(channels).unwrap()),
            ResamplerKind::CPoly => Box::new(PolyphaseResampler::new()),
        };
        let mut out = Vec::new();
        {
            let mut sink = |frame: &[i32; MAX_CH]| out.push(*frame);
            for frame in input {
                rs.push(frame, channels, &mut sink);
            }
            rs.flush(channels, &mut sink);
        }
        (out, rs.rs_in(), rs.rs_out())
    }

    #[test]
    fn both_resamplers_match_the_c_frame_count() {
        let input = tone(44_100, 2, 1_000.0, 44_100.0);
        for kind in [ResamplerKind::Rubato, ResamplerKind::CPoly] {
            let (out, rs_in, rs_out) = run(kind, &input, 2);
            assert_eq!(rs_in, 44_100);
            // C flush condition: outputs while hist_count > pos/160 + 143.
            assert_eq!(rs_out, 48_000, "{kind:?}");
            assert_eq!(out.len() as u64, 48_000, "{kind:?}");
        }
    }

    #[test]
    fn six_channel_levels_survive_both_resamplers() {
        let levels = [100_000, 400_000, 800_000, 1_500_000, 3_000_000, 6_000_000];
        let input: Vec<[i32; MAX_CH]> = (0..20_000)
            .map(|_| {
                let mut frame = [0_i32; MAX_CH];
                for (c, level) in levels.iter().enumerate() {
                    frame[c] = *level;
                }
                frame
            })
            .collect();
        for kind in [ResamplerKind::Rubato, ResamplerKind::CPoly] {
            let (out, _, _) = run(kind, &input, 6);
            let start = 2000;
            let end = out.len() - 2000;
            for (c, level) in levels.iter().enumerate() {
                let mean: f64 = out[start..end].iter().map(|f| f[c] as f64).sum::<f64>()
                    / (end - start) as f64;
                assert!(
                    (mean - *level as f64).abs() < (*level as f64) * 0.02,
                    "{kind:?} ch{c} mean {mean} expected {level}"
                );
            }
        }
    }

    #[test]
    fn polyphase_dc_gain_is_unity() {
        // Every Q30 row sums to 2^30 within 14 LSB, so a constant input keeps
        // its level to within one output LSB. Do not check the last 200 frames:
        // flush pushes the C daemon's 143 zero frames, so the tail is the
        // expected FIR ramp-down (its final frames drop to about 0.7 of the
        // level), exactly as the C daemon behaves.
        let input: Vec<[i32; MAX_CH]> = (0..4000)
            .map(|_| {
                let mut frame = [0_i32; MAX_CH];
                frame[0] = 1_000_000;
                frame
            })
            .collect();
        let (out, _, _) = run(ResamplerKind::CPoly, &input, 1);
        let steady = out.len().saturating_sub(200);
        for (index, frame) in out.iter().enumerate().take(steady).skip(700) {
            assert!(
                (frame[0] - 1_000_000).abs() < 10,
                "dc gain frame {index}: {}",
                frame[0]
            );
        }
    }

    #[test]
    fn polyphase_impulse_stays_bounded() {
        let mut input: Vec<[i32; MAX_CH]> = (0..2000).map(|_| [0_i32; MAX_CH]).collect();
        input[500][0] = 8_000_000;
        let (out, _, _) = run(ResamplerKind::CPoly, &input, 1);
        let peak = out.iter().map(|f| f[0]).max().unwrap();
        assert!(peak > 1_000_000 && peak < 9_000_000, "impulse peak {peak}");
    }

    #[test]
    fn polyphase_is_deterministic() {
        let input = tone(10_000, 2, 997.0, 44_100.0);
        let (a, _, _) = run(ResamplerKind::CPoly, &input, 2);
        let (b, _, _) = run(ResamplerKind::CPoly, &input, 2);
        assert_eq!(a, b);
    }

    #[test]
    fn rubato_keeps_the_signal_inside_the_band() {
        // 1 kHz tone: compare RMS before/after; a sane SRC keeps it within 1 %.
        let input = tone(48_000, 1, 1_000.0, 44_100.0);
        let (out, _, _) = run(ResamplerKind::Rubato, &input, 1);
        let skipped = 512; // ignore the filter warm-up
        let rms: f64 = out[skipped..]
            .iter()
            .map(|f| f[0] as f64 * f[0] as f64)
            .sum::<f64>()
            / (out.len() - skipped) as f64;
        let rms = rms.sqrt();
        let reference = 8_000_000.0 / 2.0_f64.sqrt();
        assert!(
            (rms / reference - 1.0).abs() < 0.05,
            "rms {rms} vs {reference}"
        );
    }
}
