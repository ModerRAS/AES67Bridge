//! `@aes67bridge.ctl`: runtime control/state channel (JSON lines).
//!
//! The TV app (and test tooling) can read the current last-stage trim and set
//! it, step it or mute it. Every connected client receives the current state on
//! connect and a new line whenever the state changes, so the on-screen volume
//! bar can follow the remote keys without polling.
//!
//! Protocol (one JSON object per line):
//!   client -> daemon: {"cmd":"get"}
//!                     {"cmd":"set","cdb":-1200}
//!                     {"cmd":"set","db":-12.5}
//!                     {"cmd":"step","dir":"up"|"down"}
//!                     {"cmd":"mute","on":true|false}
//!                     {"cmd":"toggle-mute"}
//!   daemon -> client: {"trim_cdb":-1200,"trim_db":-12.00,"muted":false,"step":8}

use std::sync::{Arc, Mutex, OnceLock};

use crate::trim::{Trim, STEP_CDB};

pub const SOCKET_NAME: &str = "aes67bridge.ctl";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Get,
    SetCdb(i32),
    SetDb(f32),
    StepUp,
    StepDown,
    Mute(bool),
    ToggleMute,
}

/// Current state as one JSON line (including the trailing newline).
pub fn state_json(trim: &Trim) -> String {
    let step = ((-trim.cdb()) / STEP_CDB).max(0);
    format!(
        "{{\"trim_cdb\":{},\"trim_db\":{:.2},\"muted\":{},\"step\":{}}}\n",
        trim.cdb(),
        trim.db(),
        trim.muted(),
        step
    )
}

/// Parse one JSON-lines control request. Returns `None` for malformed or
/// unknown requests (the connection stays open).
pub fn parse_command(line: &str) -> Option<Command> {
    let cmd = extract_string(line, "\"cmd\"")?;
    match cmd.as_str() {
        "get" => Some(Command::Get),
        "set" => {
            if let Some(cdb) = extract_int(line, "\"cdb\"") {
                Some(Command::SetCdb(cdb))
            } else {
                extract_float(line, "\"db\"").map(Command::SetDb)
            }
        }
        "step" => match extract_string(line, "\"dir\"")?.as_str() {
            "up" => Some(Command::StepUp),
            "down" => Some(Command::StepDown),
            _ => None,
        },
        "mute" => extract_bool(line, "\"on\"").map(Command::Mute),
        "toggle-mute" => Some(Command::ToggleMute),
        _ => None,
    }
}

/// Apply a command to the shared trim. Returns true when it changed the state.
pub fn apply(trim: &Trim, command: Command) -> bool {
    match command {
        Command::Get => false,
        Command::SetCdb(cdb) => {
            trim.set_db(cdb as f32 / 100.0);
            true
        }
        Command::SetDb(db) => {
            trim.set_db(db);
            true
        }
        Command::StepUp => {
            trim.step_up();
            true
        }
        Command::StepDown => {
            trim.step_down();
            true
        }
        Command::Mute(on) => {
            trim.set_muted(on);
            true
        }
        Command::ToggleMute => {
            trim.toggle_mute();
            true
        }
    }
}

fn field_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = line[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    Some(rest)
}

fn extract_string(line: &str, key: &str) -> Option<String> {
    let rest = field_value(line, key)?;
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_int(line: &str, key: &str) -> Option<i32> {
    let rest = field_value(line, key)?;
    let rest = rest.trim_start_matches(&['"', '[', ' '][..]);
    let end = rest
        .find(|c: char| !(c == '-' || c.is_ascii_digit()))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_float(line: &str, key: &str) -> Option<f32> {
    let rest = field_value(line, key)?;
    let rest = rest.trim_start_matches(['"', '[', ' ']);
    let end = rest
        .find(|c: char| !(c == '-' || c == '+' || c == '.' || c.is_ascii_digit()))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_bool(line: &str, key: &str) -> Option<bool> {
    let rest = field_value(line, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

struct Hub {
    trim: Arc<Trim>,
    clients: Mutex<Vec<std::sync::mpsc::SyncSender<String>>>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

/// Notify every connected client with the current state.
pub fn broadcast() {
    let Some(hub) = HUB.get() else {
        return;
    };
    let text = state_json(&hub.trim);
    let mut clients = match hub.clients.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    clients.retain(|tx| match tx.try_send(text.clone()) {
        Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => true,
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
    });
}

#[cfg(unix)]
mod socket {
    use super::{apply, broadcast, parse_command, state_json, Hub, HUB, SOCKET_NAME};
    use crate::trim::Trim;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::io::FromRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    };
    use std::thread;

    fn bind_abstract(name: &str) -> std::io::Result<UnixListener> {
        let bytes = name.as_bytes();
        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            if bytes.len() + 1 > addr.sun_path.len() {
                libc::close(fd);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "socket name too long",
                ));
            }
            for (i, byte) in bytes.iter().enumerate() {
                addr.sun_path[i + 1] = *byte as libc::c_char;
            }
            let len =
                (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
            if libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) != 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            if libc::listen(fd, 8) != 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            Ok(UnixListener::from_raw_fd(fd))
        }
    }

    fn spawn_client(stream: UnixStream, hub: &'static Hub) {
        let Ok(read_half) = stream.try_clone() else {
            return;
        };
        let (tx, rx): (SyncSender<String>, Receiver<String>) = sync_channel(8);
        let _ = tx.try_send(state_json(&hub.trim));
        if let Ok(mut clients) = hub.clients.lock() {
            clients.push(tx);
        }
        let mut write_half = stream;
        let writer = thread::Builder::new()
            .name("aes67-ctl-w".into())
            .spawn(move || {
                while let Ok(line) = rx.recv() {
                    if write_half.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                }
            });
        let reader = thread::Builder::new()
            .name("aes67-ctl-r".into())
            .spawn(move || {
                let reader = BufReader::new(read_half);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let Some(command) = parse_command(&line) else {
                        continue;
                    };
                    if apply(&hub.trim, command) {
                        broadcast();
                    }
                }
            });
        // Both threads end on EOF/write error; detach them.
        let _ = writer;
        let _ = reader;
    }

    pub fn init(trim: Arc<Trim>) -> std::io::Result<()> {
        let listener = bind_abstract(SOCKET_NAME)?;
        let _ = HUB.set(Hub {
            trim,
            clients: Mutex::new(Vec::new()),
        });
        let _ = thread::Builder::new()
            .name("aes67-ctl".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            if let Some(hub) = HUB.get() {
                                spawn_client(stream, hub);
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        Ok(())
    }
}

#[cfg(not(unix))]
mod socket {
    use super::Arc;
    use crate::trim::Trim;
    use std::io;

    pub fn init(_trim: Arc<Trim>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "control socket is unix-only",
        ))
    }
}

pub use socket::init;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get_set_step_and_mute() {
        assert_eq!(parse_command("{\"cmd\":\"get\"}"), Some(Command::Get));
        assert_eq!(
            parse_command("{\"cmd\":\"set\",\"cdb\":-1200}"),
            Some(Command::SetCdb(-1200))
        );
        match parse_command("{\"cmd\":\"set\",\"db\":-12.5}") {
            Some(Command::SetDb(db)) => assert!((db + 12.5).abs() < 1e-6),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_command("{\"cmd\":\"step\",\"dir\":\"down\"}"),
            Some(Command::StepDown)
        );
        assert_eq!(
            parse_command("{\"cmd\":\"mute\",\"on\":true}"),
            Some(Command::Mute(true))
        );
        assert_eq!(parse_command("{\"cmd\":\"nope\"}"), None);
        assert_eq!(parse_command("not json"), None);
    }

    #[test]
    fn applies_set_and_reports_state() {
        let trim = Trim::new(None);
        assert!(apply(&trim, Command::SetCdb(-1250)));
        assert_eq!(trim.cdb(), -1250);
        let state = state_json(&trim);
        assert!(state.contains("\"trim_cdb\":-1250"), "{state}");
        assert!(state.contains("\"trim_db\":-12.50"), "{state}");
        assert!(state.contains("\"muted\":false"), "{state}");
        assert!(apply(&trim, Command::StepUp));
        assert_eq!(trim.cdb(), -1100);
        assert!(apply(&trim, Command::ToggleMute));
        assert!(trim.muted());
    }
}
