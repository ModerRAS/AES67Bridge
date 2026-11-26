//! Runtime output trim (remote-control volume) at the last stage.
//!
//! The configured `gain_db` is applied inside every render path, before the
//! ROAR limiter (headroom / dialogue normalisation). The remote-control volume
//! is a different knob: it is a runtime attenuation applied to the final f32
//! samples, right before they enter the `nsp_aes67` session ring. This is the
//! last point the daemon touches audio, so one implementation covers PCM,
//! E-AC-3 and TrueHD/Atmos without touching the vendored transport.
//!
//! Because it sits after the limiter it may only attenuate: the range is
//! 0 dB (no trim) down to -60 dB, and one more step below that mutes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use nsp_aes67::session::Session;

/// Default state file: the app's private dir (same persistent layer as
/// `daemon.conf`; `/data/aes67bridge` is tmpfs since 0.3.0).
pub const DEFAULT_STATE: &str = "/data/user/0/dev.aes67bridge.tv/files/volume.json";

pub const MAX_CDB: i32 = 0;
pub const MIN_CDB: i32 = -6_000;
/// One volume-key step: 1.5 dB.
pub const STEP_CDB: i32 = 150;

/// Fraction of the remaining gain difference consumed per submitted block
/// (~10 ms at 48 kHz/480 frames), i.e. a ~30 ms ramp.
const RAMP_FRACTION: f32 = 0.5;

/// Last-stage attenuation shared by every render path.
pub struct Trim {
    target_cdb: AtomicI32,
    muted: AtomicBool,
    state_path: Option<PathBuf>,
    runtime: Mutex<Runtime>,
}

struct Runtime {
    /// Current linear gain (1.0 = untouched, 0.0 = silence).
    current_lin: f32,
}

impl Trim {
    /// Load the persisted level (if any) from `state_path`. A missing or
    /// malformed file starts at 0 dB / unmuted.
    pub fn new(state_path: Option<PathBuf>) -> Self {
        let trim = Self {
            target_cdb: AtomicI32::new(MAX_CDB),
            muted: AtomicBool::new(false),
            state_path,
            runtime: Mutex::new(Runtime { current_lin: 1.0 }),
        };
        trim.load();
        trim
    }

    /// Persisted path used when AES67_VOLUME_STATE is not set.
    pub fn default_path() -> Option<PathBuf> {
        match std::env::var("AES67_VOLUME_STATE") {
            Ok(value) if value == "off" => None,
            Ok(value) if !value.is_empty() => Some(PathBuf::from(value)),
            _ => Some(PathBuf::from(DEFAULT_STATE)),
        }
    }

    /// Target level in dB (0.0 .. -60.0). Looks the same while muted.
    pub fn db(&self) -> f32 {
        self.target_cdb.load(Ordering::Relaxed) as f32 / 100.0
    }

    pub fn cdb(&self) -> i32 {
        self.target_cdb.load(Ordering::Relaxed)
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Linear gain currently requested (0.0 when muted).
    pub fn target_linear(&self) -> f32 {
        if self.muted() {
            0.0
        } else {
            db_to_linear(self.db())
        }
    }

    /// Absolute level in dB, clamped to the supported range.
    pub fn set_db(&self, db: f32) {
        if !db.is_finite() {
            return;
        }
        let cdb = ((db * 100.0).round() as i32).clamp(MIN_CDB, MAX_CDB);
        if self.target_cdb.swap(cdb, Ordering::Relaxed) != cdb {
            self.persist();
        }
    }

    /// One step louder. While muted the first press only unmutes (it keeps
    /// the level instead of jumping a step); at 0 dB it is a no-op.
    pub fn step_up(&self) {
        if self.muted.swap(false, Ordering::Relaxed) {
            self.persist();
            return;
        }
        let cdb = self.target_cdb.load(Ordering::Relaxed);
        if cdb < MAX_CDB {
            let next = (cdb + STEP_CDB).min(MAX_CDB);
            self.target_cdb.store(next, Ordering::Relaxed);
            self.persist();
        }
    }

    /// One step quieter; below -60 dB the next press mutes.
    pub fn step_down(&self) {
        if self.muted() {
            return;
        }
        let cdb = self.target_cdb.load(Ordering::Relaxed);
        if cdb <= MIN_CDB {
            self.muted.store(true, Ordering::Relaxed);
        } else {
            let next = (cdb - STEP_CDB).max(MIN_CDB);
            self.target_cdb.store(next, Ordering::Relaxed);
        }
        self.persist();
    }

    pub fn set_muted(&self, muted: bool) {
        if self.muted.swap(muted, Ordering::Relaxed) != muted {
            self.persist();
        }
    }

    pub fn toggle_mute(&self) {
        self.set_muted(!self.muted());
    }

    /// Apply the trim in place to one interleaved f32 block.
    ///
    /// Returns true when the samples were modified. At 0 dB with a settled
    /// ramp the call is a no-op (no copy, no traversal), so the common
    /// "no volume change" case costs nothing.
    pub fn apply(&self, samples: &mut [f32], frames: usize, channels: usize) -> bool {
        let Some(n) = frames.checked_mul(channels) else {
            return false;
        };
        if n == 0 || samples.len() < n {
            return false;
        }
        let target = self.target_linear();
        let (start, end) = {
            let mut runtime = match self.runtime.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let start = runtime.current_lin;
            let mut end = start + (target - start) * RAMP_FRACTION;
            if (target - end).abs() <= 1.0e-4 {
                end = target;
            }
            runtime.current_lin = end;
            (start, end)
        };
        if start == 1.0 && end == 1.0 {
            return false;
        }
        if start == end {
            for sample in &mut samples[..n] {
                *sample *= start;
            }
        } else {
            let step = (end - start) / n as f32;
            let mut gain = start;
            for sample in &mut samples[..n] {
                *sample *= gain;
                gain += step;
            }
        }
        true
    }

    fn load(&self) {
        let Some(path) = self.state_path.as_deref() else {
            return;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let cdb = parse_int(&text, "\"trim_cdb\":")
            .unwrap_or(MAX_CDB)
            .clamp(MIN_CDB, MAX_CDB);
        let muted = parse_bool(&text, "\"muted\":").unwrap_or(false);
        self.target_cdb.store(cdb, Ordering::Relaxed);
        self.muted.store(muted, Ordering::Relaxed);
        // Start the ramp at the restored gain instead of fading in from 1.0.
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.current_lin = if muted {
                0.0
            } else {
                db_to_linear(cdb as f32 / 100.0)
            };
        }
    }

    fn persist(&self) {
        let Some(path) = self.state_path.as_deref() else {
            return;
        };
        self.write_state(path);
    }

    fn write_state(&self, path: &Path) {
        let body = format!(
            "{{\"trim_cdb\":{},\"muted\":{}}}\n",
            self.cdb(),
            self.muted()
        );
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, body.as_bytes()).is_err() {
            return;
        }
        let _ = fs::rename(&tmp, path);
    }
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear gain for the current trim, used by tests/diagnostics.
pub fn linear_for_cdb(cdb: i32) -> f32 {
    db_to_linear(cdb as f32 / 100.0)
}

fn parse_int(text: &str, key: &str) -> Option<i32> {
    let start = text.find(key)? + key.len();
    let rest = text[start..].trim_start();
    let end = rest
        .find(|c: char| !(c == '-' || c.is_ascii_digit()))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn parse_bool(text: &str, key: &str) -> Option<bool> {
    let start = text.find(key)? + key.len();
    let rest = text[start..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Submit one block through the shared last-stage trim.
///
/// # Safety
/// `session` must be a live nsp_aes67 session and `samples` must hold
/// `frames * channels` f32 samples.
pub unsafe fn submit_trimmed(
    session: &Session,
    trim: &Trim,
    first: u64,
    samples: &mut [f32],
    frames: usize,
    channels: usize,
) -> i32 {
    let _ = trim.apply(samples, frames, channels);
    unsafe { session.submit(first, samples.as_ptr(), frames, channels) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aes67-trim-{}-{}-{}.json",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn settle(trim: &Trim, blocks: usize) -> Vec<f32> {
        let mut samples = vec![0.0f32; 16];
        for _ in 0..blocks {
            samples = vec![1.0f32; 16];
            trim.apply(&mut samples, 8, 2);
        }
        samples
    }

    #[test]
    fn zero_db_is_a_no_op() {
        let trim = Trim::new(None);
        let mut samples = vec![0.25f32; 32];
        assert!(!trim.apply(&mut samples, 16, 2));
        assert_eq!(samples, vec![0.25f32; 32]);
    }

    #[test]
    fn attenuation_ramps_to_the_target_without_a_jump() {
        let trim = Trim::new(None);
        trim.set_db(-6.0);
        // The block starts at the current gain (no click) and ramps down;
        // it must not jump straight to the -6 dB target.
        let mut first = vec![1.0f32; 480];
        trim.apply(&mut first, 240, 2);
        assert_eq!(first[0], 1.0);
        let last = first[first.len() - 1];
        assert!(last < 1.0);
        assert!(last > 0.50, "last sample {last} jumped past the target");
        // After ~100 ms the gain has settled at the exact target.
        let settled = settle(&trim, 20);
        let expected = db_to_linear(-6.0);
        assert!(
            (settled[0] - expected).abs() < 1.0e-3,
            "{} vs {}",
            settled[0],
            expected
        );
    }

    #[test]
    fn mute_reaches_silence_and_unmute_recovers() {
        let trim = Trim::new(None);
        trim.set_db(-3.0);
        let _ = settle(&trim, 10);
        trim.set_muted(true);
        let muted = settle(&trim, 20);
        assert!(muted.iter().all(|sample| *sample == 0.0), "{muted:?}");
        trim.set_muted(false);
        let restored = settle(&trim, 20);
        assert!((restored[0] - db_to_linear(-3.0)).abs() < 1.0e-3);
    }

    #[test]
    fn steps_clamp_and_bottom_step_enters_mute() {
        let trim = Trim::new(None);
        for _ in 0..40 {
            trim.step_down();
        }
        assert_eq!(trim.cdb(), MIN_CDB);
        assert!(!trim.muted());
        trim.step_down();
        assert!(trim.muted());
        // Stepping up unmutes and keeps the level.
        trim.step_up();
        assert!(!trim.muted());
        assert_eq!(trim.cdb(), MIN_CDB);
        for _ in 0..40 {
            trim.step_up();
        }
        assert_eq!(trim.cdb(), MAX_CDB);
    }

    #[test]
    fn state_round_trips_through_the_file() {
        let path = temp_path("roundtrip");
        let trim = Trim::new(Some(path.clone()));
        trim.set_db(-12.0);
        trim.set_muted(false);
        let reloaded = Trim::new(Some(path.clone()));
        assert_eq!(reloaded.cdb(), -1_200);
        assert!(!reloaded.muted());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_state_falls_back_to_zero() {
        let path = temp_path("malformed");
        fs::write(&path, "{not json").unwrap();
        let trim = Trim::new(Some(path.clone()));
        assert_eq!(trim.cdb(), 0);
        assert!(!trim.muted());
        let _ = fs::remove_file(path);
    }
}
