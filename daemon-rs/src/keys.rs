//! Passive remote-control volume keys (`/dev/input/event*`).
//!
//! The HAL tee taps PCM *before* the vendor volume stage, so the Android
//! volume keys do not change the AES67 level (and in bitstream they change
//! nothing at all). The daemon runs as root, so it reads the kernel input
//! devices read-only and translates KEY_VOLUMEUP / KEY_VOLUMEDOWN / KEY_MUTE
//! into trim steps. The devices are never grabbed: the system keeps receiving
//! every key, we only observe.
//!
//! IR, Bluetooth remotes and CEC all surface as input key events on the Z9X
//! (`ir_keypad`, `Remote-V12`, `cec_input`), so one reader covers all three.

use std::sync::Arc;

use crate::trim::Trim;

pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_MUTE: u16 = 113;

/// Minimum gap between two auto-repeat steps (value == 2) from one device.
const REPEAT_INTERVAL_MS: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Up,
    Down,
    Mute,
}

/// Map one evdev event to a volume action. Only key presses (value 1) and
/// auto-repeats (value 2) count; releases are ignored.
pub fn action_for(code: u16, value: i32) -> Option<KeyAction> {
    if value != 1 && value != 2 {
        return None;
    }
    match code {
        KEY_VOLUMEUP => Some(KeyAction::Up),
        KEY_VOLUMEDOWN => Some(KeyAction::Down),
        KEY_MUTE => Some(KeyAction::Mute),
        _ => None,
    }
}

/// Start the key reader. Returns Ok immediately; on non-Linux/Android targets
/// this is a no-op (the daemon is only deployed on Android).
pub fn init(trim: Arc<Trim>) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        linux::start(trim)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = trim;
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux {
    use super::{action_for, KeyAction, REPEAT_INTERVAL_MS};
    use crate::trim::Trim;
    use std::collections::HashSet;
    use std::fs::File;
    use std::io::Read;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Kernel `struct input_event` without pulling in libc's time_t width
    /// assumptions: 32-bit ARM/Android uses two 32-bit longs, 64-bit hosts use
    /// two 64-bit longs.
    #[cfg(target_pointer_width = "32")]
    #[repr(C)]
    struct InputEvent {
        sec: i32,
        usec: i32,
        type_: u16,
        code: u16,
        value: i32,
    }

    #[cfg(target_pointer_width = "64")]
    #[repr(C)]
    struct InputEvent {
        sec: i64,
        usec: i64,
        type_: u16,
        code: u16,
        value: i32,
    }

    const EV_KEY: u16 = 1;

    pub fn start(trim: Arc<Trim>) -> std::io::Result<()> {
        let active: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        thread::Builder::new()
            .name("aes67-keys".into())
            .spawn(move || loop {
                scan(&trim, &active);
                thread::sleep(Duration::from_secs(2));
            })?;
        Ok(())
    }

    fn scan(trim: &Arc<Trim>, active: &Arc<Mutex<HashSet<String>>>) {
        let Ok(entries) = std::fs::read_dir("/dev/input") else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("event") {
                continue;
            }
            let path = entry.path();
            let key = path.to_string_lossy().to_string();
            {
                let Ok(mut set) = active.lock() else {
                    return;
                };
                if !set.insert(key.clone()) {
                    continue;
                }
            }
            let thread_trim = Arc::clone(trim);
            let thread_active = Arc::clone(active);
            let key_for_cleanup = key.clone();
            let spawned = thread::Builder::new()
                .name("aes67-keydev".into())
                .spawn(move || {
                    read_device(&path, &thread_trim);
                    if let Ok(mut set) = thread_active.lock() {
                        set.remove(&key_for_cleanup);
                    }
                });
            if spawned.is_err() {
                if let Ok(mut set) = active.lock() {
                    set.remove(&key);
                }
            }
        }
    }

    fn read_device(path: &Path, trim: &Trim) {
        let Ok(mut file) = File::open(path) else {
            return;
        };
        let mut last_repeat: Option<Instant> = None;
        loop {
            let mut event: InputEvent = InputEvent {
                sec: 0,
                usec: 0,
                type_: 0,
                code: 0,
                value: 0,
            };
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut event as *mut InputEvent as *mut u8,
                    std::mem::size_of::<InputEvent>(),
                )
            };
            if file.read_exact(bytes).is_err() {
                return;
            }
            if event.type_ != EV_KEY {
                continue;
            }
            let Some(action) = action_for(event.code, event.value) else {
                continue;
            };
            if event.value == 2 {
                if let Some(last) = last_repeat {
                    if last.elapsed() < Duration::from_millis(REPEAT_INTERVAL_MS) {
                        continue;
                    }
                }
                last_repeat = Some(Instant::now());
            } else {
                last_repeat = None;
            }
            match action {
                KeyAction::Up => trim.step_up(),
                KeyAction::Down => trim.step_down(),
                KeyAction::Mute => trim.toggle_mute(),
            }
            crate::ctl::broadcast();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_volume_keys_and_ignores_releases() {
        assert_eq!(action_for(KEY_VOLUMEUP, 1), Some(KeyAction::Up));
        assert_eq!(action_for(KEY_VOLUMEDOWN, 1), Some(KeyAction::Down));
        assert_eq!(action_for(KEY_MUTE, 1), Some(KeyAction::Mute));
        assert_eq!(action_for(KEY_VOLUMEUP, 2), Some(KeyAction::Up));
        assert_eq!(action_for(KEY_VOLUMEUP, 0), None);
        assert_eq!(action_for(KEY_POWER, 1), None);
    }

    const KEY_POWER: u16 = 116;
}
