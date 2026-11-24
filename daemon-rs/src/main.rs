//! aes67-daemon entry point.

use aes67d::config::parse_args;
use aes67d::daemon::Daemon;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg = match parse_args(&args) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => return,
        Err(message) => {
            eprintln!("ERROR {message}");
            std::process::exit(2);
        }
    };
    match Daemon::new(cfg) {
        Ok(mut daemon) => {
            let code = daemon.run();
            std::process::exit(code);
        }
        Err(message) => {
            eprintln!("ERROR {message}");
            std::process::exit(2);
        }
    }
}
