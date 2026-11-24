//! Tiny logger: stderr plus an optional append-only file, line buffered and
//! flushed per line (the C daemon logs to stderr, redirected by the start script).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<File>> {
    SINK.get_or_init(|| Mutex::new(None))
}

pub fn set_file(path: &str) -> std::io::Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    if let Ok(mut guard) = sink().lock() {
        *guard = Some(file);
    }
    Ok(())
}

pub fn write_line(line: &str) {
    eprintln!("{line}");
    if let Ok(mut guard) = sink().lock() {
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::logging::write_line(&format!($($arg)*))
    };
}
