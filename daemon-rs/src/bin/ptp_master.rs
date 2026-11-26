//! Test-only PTPv2 leader (AES67 media profile / SMPTE ST 2059-2).
//!
//! Sends Announce, Sync and Follow_Up to one follower and answers Delay_Req
//! with Delay_Resp, i.e. the subset of the leader an ordinary clock needs to
//! lock the daemon's PTP follower when no broadcast grandmaster is reachable.
//! It exists for conformance tests (ST 2110-30 / ST 2059-2 alignment) and is not
//! part of the product path. Defaults are the ST 2059-2 values: domain 127,
//! announce 1 s (log 0), Sync 125 ms (log -3), two-step clock.
//!
//! Run it on the same host as the test daemon instance with non-default ports
//! so the production daemon's 319/320 sockets stay untouched, e.g.:
//!
//!   aes67-ptp-master --interface 192.168.63.158 --follower 192.168.63.158 \
//!     --event-port 1319 --general-port 1320 \
//!     --follower-event-port 2319 --follower-general-port 2320 \
//!     --domain 127 --clock-id 02-11-22-33-44-55-66-77

use nsp_aes67::ptp::codec::{encode, Body, ClockId, Message, PTP_TIMESCALE, TWO_STEP};
use std::env;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

struct Args {
    interface: Ipv4Addr,
    follower: Ipv4Addr,
    follower_event_port: u16,
    follower_general_port: u16,
    event_port: u16,
    general_port: u16,
    domain: u8,
    announce_log: i8,
    sync_log: i8,
    one_step: bool,
    seconds: u64,
    clock_id: ClockId,
}

fn parse_ipv4(text: &str) -> Ipv4Addr {
    text.parse().unwrap_or_else(|_| {
        eprintln!("bad IPv4 address '{text}'");
        std::process::exit(2)
    })
}

fn parse_clock_id(text: &str) -> ClockId {
    let mut id = [0u8; 8];
    let parts: Vec<&str> = text.split(['-', ':']).collect();
    if parts.len() != 8 {
        eprintln!("--clock-id needs eight hex octets, e.g. 02-11-22-33-44-55-66-77");
        std::process::exit(2);
    }
    for (slot, part) in id.iter_mut().zip(parts) {
        *slot = u8::from_str_radix(part, 16).unwrap_or_else(|_| {
            eprintln!("bad --clock-id octet '{part}'");
            std::process::exit(2)
        });
    }
    id
}

fn parse_args() -> Args {
    let mut args = Args {
        interface: Ipv4Addr::LOCALHOST,
        follower: Ipv4Addr::LOCALHOST,
        follower_event_port: 319,
        follower_general_port: 320,
        event_port: 319,
        general_port: 320,
        domain: 127,
        announce_log: 0,
        sync_log: -3,
        one_step: false,
        seconds: 0,
        clock_id: [0x02, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
    };
    let mut index = 1;
    while index < args_len() {
        let flag = arg_at(index);
        let mut value = |name: &str| {
            if index + 1 >= args_len() {
                eprintln!("{name} needs a value");
                std::process::exit(2);
            }
            index += 1;
            arg_at(index)
        };
        match flag.as_str() {
            "--interface" => args.interface = parse_ipv4(&value("--interface")),
            "--follower" => args.follower = parse_ipv4(&value("--follower")),
            "--event-port" => args.event_port = value("--event-port").parse().unwrap(),
            "--general-port" => args.general_port = value("--general-port").parse().unwrap(),
            "--follower-event-port" => {
                args.follower_event_port = value("--follower-event-port").parse().unwrap()
            }
            "--follower-general-port" => {
                args.follower_general_port = value("--follower-general-port").parse().unwrap()
            }
            "--domain" => args.domain = value("--domain").parse().unwrap(),
            "--announce-log" => args.announce_log = value("--announce-log").parse().unwrap(),
            "--sync-log" => args.sync_log = value("--sync-log").parse().unwrap(),
            "--one-step" => args.one_step = true,
            "--seconds" => args.seconds = value("--seconds").parse().unwrap(),
            "--clock-id" => args.clock_id = parse_clock_id(&value("--clock-id")),
            other => {
                eprintln!("unknown argument '{other}'");
                eprintln!(
                    "usage: aes67-ptp-master --interface IP --follower IP \
                     [--event-port 319] [--general-port 320] \
                     [--follower-event-port 319] [--follower-general-port 320] \
                     [--domain 127] [--announce-log 0] [--sync-log -3] [--one-step] \
                     [--seconds N] [--clock-id 02-11-22-33-44-55-66-77]"
                );
                std::process::exit(2);
            }
        }
        index += 1;
    }
    args
}

// Tiny argv accessors keep the borrow checker happy inside the value closure.
fn args_vec() -> &'static Vec<String> {
    use std::sync::OnceLock;
    static ARGS: OnceLock<Vec<String>> = OnceLock::new();
    ARGS.get_or_init(|| env::args().collect())
}
fn args_len() -> usize {
    args_vec().len()
}
fn arg_at(index: usize) -> String {
    args_vec()[index].clone()
}

fn interval_ns(log: i8) -> i128 {
    1_000_000_000i128 >> (-log).max(0) as u32
}

fn main() {
    let args = parse_args();
    let event = UdpSocket::bind((args.interface, args.event_port)).unwrap_or_else(|err| {
        eprintln!("bind event {}:{}: {err}", args.interface, args.event_port);
        std::process::exit(1)
    });
    let general = UdpSocket::bind((args.interface, args.general_port)).unwrap_or_else(|err| {
        eprintln!(
            "bind general {}:{}: {err}",
            args.interface, args.general_port
        );
        std::process::exit(1)
    });
    event.set_nonblocking(true).unwrap();
    let _ = event.set_read_timeout(Some(Duration::from_millis(2)));
    println!(
        "ptp-master domain={} clock={:02X?} announce={}ms sync={}ms {} step -> follower {}:{}/{}",
        args.domain,
        args.clock_id,
        interval_ns(args.announce_log) / 1_000_000,
        interval_ns(args.sync_log) / 1_000_000,
        if args.one_step { "one" } else { "two" },
        args.follower,
        args.follower_event_port,
        args.follower_general_port
    );

    let mut source = [0u8; 10];
    source[..8].copy_from_slice(&args.clock_id);
    source[9] = 1;
    // Arbitrary but fixed PTP epoch for the test (2025-10-01T00:00:00Z-ish).
    let epoch: i128 = 1_900_000_000_000_000_000;
    let start = Instant::now();
    let ptp_now = |now: Instant| epoch + now.duration_since(start).as_nanos() as i128;

    let mut announce_seq = 0u16;
    let mut sync_seq = 0u16;
    let mut next_announce = 0i128;
    let mut next_sync = 0i128;
    let mut buf = [0u8; 256];
    let announce_interval = interval_ns(args.announce_log);
    let sync_interval = interval_ns(args.sync_log);
    let deadline = (args.seconds > 0).then(|| start + Duration::from_secs(args.seconds));
    let mut sent_announce = 0u64;
    let mut sent_sync = 0u64;
    let mut answered_delay = 0u64;

    loop {
        let now = Instant::now();
        if deadline.is_some_and(|end| now >= end) {
            break;
        }
        let ptp = ptp_now(now);
        if ptp >= next_announce {
            let msg = Message {
                domain: args.domain,
                source,
                seq: announce_seq,
                flags: PTP_TIMESCALE,
                log_interval: args.announce_log,
                correction_scaled_ns: 0,
                body: Body::Announce {
                    grandmaster: args.clock_id,
                },
            };
            announce_seq = announce_seq.wrapping_add(1);
            let mut bytes = [0u8; 64];
            let wire = encode(msg, &mut bytes);
            let _ = general.send_to(
                wire,
                SocketAddrV4::new(args.follower, args.follower_general_port),
            );
            sent_announce += 1;
            next_announce = (ptp - next_announce) / announce_interval * announce_interval
                + next_announce
                + announce_interval;
        }
        if ptp >= next_sync {
            let origin = ptp;
            let (body, flags) = if args.one_step {
                (Body::SyncOneStep(origin), 0)
            } else {
                (Body::Sync, TWO_STEP)
            };
            let sync = Message {
                domain: args.domain,
                source,
                seq: sync_seq,
                flags,
                log_interval: args.sync_log,
                correction_scaled_ns: 0,
                body,
            };
            let mut bytes = [0u8; 64];
            let wire = encode(sync, &mut bytes);
            let _ = event.send_to(
                wire,
                SocketAddrV4::new(args.follower, args.follower_event_port),
            );
            if !args.one_step {
                let follow = Message {
                    domain: args.domain,
                    source,
                    seq: sync_seq,
                    flags: 0,
                    log_interval: args.sync_log,
                    correction_scaled_ns: 0,
                    body: Body::FollowUp(origin),
                };
                let mut follow_bytes = [0u8; 64];
                let wire = encode(follow, &mut follow_bytes);
                let _ = general.send_to(
                    wire,
                    SocketAddrV4::new(args.follower, args.follower_general_port),
                );
            }
            sync_seq = sync_seq.wrapping_add(1);
            sent_sync += 1;
            next_sync =
                (ptp - next_sync) / sync_interval * sync_interval + next_sync + sync_interval;
        }
        // Answer every Delay_Req with a Delay_Resp on the general socket.
        loop {
            match event.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if let Ok(message) = nsp_aes67::ptp::codec::decode(&buf[..n]) {
                        if let Body::DelayReq(_) = message.body {
                            let reply = Message {
                                domain: args.domain,
                                source,
                                seq: message.seq,
                                flags: 0,
                                log_interval: 0x7f,
                                correction_scaled_ns: 0,
                                body: Body::DelayResp {
                                    received_ns: ptp_now(Instant::now()),
                                    requester: message.source,
                                },
                            };
                            let mut reply_bytes = [0u8; 64];
                            let wire = encode(reply, &mut reply_bytes);
                            let _ = general.send_to(
                                wire,
                                SocketAddrV4::new(args.follower, args.follower_general_port),
                            );
                            answered_delay += 1;
                            let _ = from;
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        std::thread::sleep(Duration::from_micros(500));
        if (sent_announce + sent_sync) % 800 == 0 && sent_sync > 0 {
            println!(
                "sent announce={sent_announce} sync={sent_sync} delay_resp={answered_delay} ptp_ns={ptp}"
            );
        }
    }
    println!("done announce={sent_announce} sync={sent_sync} delay_resp={answered_delay}");
}
