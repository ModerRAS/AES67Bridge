//! aes67-ctl: read/control the running daemon over `@aes67bridge.ctl`.
//!
//! Usage:
//!   aes67-ctl [get]                print the current trim state
//!   aes67-ctl set -12.5            absolute level in dB (0 .. -60)
//!   aes67-ctl set-cdb -1250        absolute level in centi-dB
//!   aes67-ctl step up|down         one 1.5 dB step
//!   aes67-ctl mute on|off          mute / unmute
//!   aes67-ctl toggle-mute
//!   aes67-ctl watch                print every state change (Ctrl-C to stop)

#[cfg(unix)]
fn main() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::io::FromRawFd;

    const SOCKET: &str = "aes67bridge.ctl";

    fn connect_abstract(name: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
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
            if libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len) != 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            Ok(std::os::unix::net::UnixStream::from_raw_fd(fd))
        }
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match args.first().map(String::as_str) {
        None | Some("get") => "{\"cmd\":\"get\"}".to_string(),
        Some("set") => match args.get(1).and_then(|v| v.parse::<f32>().ok()) {
            Some(db) => format!("{{\"cmd\":\"set\",\"db\":{db}}}"),
            None => {
                eprintln!("usage: aes67-ctl set <db>");
                std::process::exit(2);
            }
        },
        Some("set-cdb") => match args.get(1).and_then(|v| v.parse::<i32>().ok()) {
            Some(cdb) => format!("{{\"cmd\":\"set\",\"cdb\":{cdb}}}"),
            None => {
                eprintln!("usage: aes67-ctl set-cdb <cdb>");
                std::process::exit(2);
            }
        },
        Some("step") => {
            let dir = args.get(1).map(String::as_str).unwrap_or("down");
            if dir != "up" && dir != "down" {
                eprintln!("usage: aes67-ctl step up|down");
                std::process::exit(2);
            }
            format!("{{\"cmd\":\"step\",\"dir\":\"{dir}\"}}")
        }
        Some("mute") => {
            let on = match args.get(1).map(String::as_str) {
                Some("on") | None => true,
                Some("off") => false,
                Some(other) => {
                    eprintln!("usage: aes67-ctl mute on|off (got {other})");
                    std::process::exit(2);
                }
            };
            format!("{{\"cmd\":\"mute\",\"on\":{on}}}")
        }
        Some("toggle-mute") => "{\"cmd\":\"toggle-mute\"}".to_string(),
        Some("watch") => "{\"cmd\":\"get\"}".to_string(),
        Some(other) => {
            eprintln!("unknown command {other}");
            std::process::exit(2);
        }
    };
    let watch = args.first().map(String::as_str) == Some("watch");

    let mut stream = match connect_abstract(SOCKET) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("connect @{SOCKET}: {err}");
            std::process::exit(1);
        }
    };
    if stream.write_all(command.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
        eprintln!("send failed");
        std::process::exit(1);
    }
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                print!("{line}");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                if !watch {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("aes67-ctl is unix-only (abstract @aes67bridge.ctl)");
    std::process::exit(2);
}
