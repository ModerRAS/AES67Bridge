//! Small OS layer: monotonic clock, absolute sleep with spin tail, signals,
//! single-instance lock, socket option helpers and 0644 file mode.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};

/// Absolute sleep lead (ns) before the busy-wait tail. Matches the C daemon's 200 us.
const SPIN_TAIL_NS: i64 = 200_000;

static STOP: AtomicBool = AtomicBool::new(false);

pub fn stop_requested() -> bool {
    STOP.load(Ordering::Acquire)
}

pub fn request_stop() {
    STOP.store(true, Ordering::Release);
}

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

#[cfg(windows)]
unsafe extern "system" fn on_ctrl(_: u32) -> i32 {
    STOP.store(true, Ordering::Release);
    1
}

/// Install SIGINT/SIGTERM (unix) or Ctrl-C (windows) handlers.
pub fn install_stop_handlers() {
    #[cfg(unix)]
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as usize;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<unsafe extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

pub fn init_timer_resolution() {
    #[cfg(unix)]
    unsafe {
        // Android/Linux default timer slack is 50 us, which is 40 % of a 125 us
        // (ST 2110-30 Level B) packet interval: a nanosleep-based pacer then
        // wakes late and the session drops slots. Ask the kernel to wake at the
        // requested time (same intent as timeBeginPeriod(1) on Windows).
        libc::prctl(libc::PR_SET_TIMERSLACK, 1, 0, 0, 0);
    }
    #[cfg(windows)]
    unsafe {
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(period: u32) -> u32;
        }
        timeBeginPeriod(1);
    }
}

/// Monotonic nanoseconds. CLOCK_MONOTONIC on unix/Android; a process-local
/// Instant base on Windows (only relative pacing matters on the host).
pub fn now_ns() -> i64 {
    #[cfg(unix)]
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
    }
    #[cfg(not(unix))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static BASE: OnceLock<Instant> = OnceLock::new();
        BASE.get_or_init(Instant::now).elapsed().as_nanos() as i64
    }
}

/// Wall clock milliseconds since the Unix epoch (status.json "ts").
pub fn realtime_ms() -> i64 {
    #[cfg(unix)]
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        ts.tv_sec as i64 * 1000 + ts.tv_nsec as i64 / 1_000_000
    }
    #[cfg(not(unix))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

pub fn sleep_us(us: u32) {
    std::thread::sleep(std::time::Duration::from_micros(u64::from(us)));
}

fn sleep_until_coarse(due_ns: i64) {
    if due_ns <= now_ns() {
        return;
    }
    #[cfg(unix)]
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        ts.tv_sec = (due_ns / 1_000_000_000) as libc::time_t;
        ts.tv_nsec = (due_ns % 1_000_000_000) as libc::c_long;
        while libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &ts, std::ptr::null_mut()) == libc::EINTR {}
    }
    #[cfg(not(unix))]
    {
        let delta = due_ns - now_ns();
        if delta > 0 {
            std::thread::sleep(std::time::Duration::from_nanos(delta as u64));
        }
    }
}

/// Sleep to an absolute monotonic deadline, then spin for the final 200 us.
pub fn sleep_until(due_ns: i64) {
    let now = now_ns();
    if due_ns <= now {
        return;
    }
    if due_ns - now > SPIN_TAIL_NS + 100_000 {
        sleep_until_coarse(due_ns - SPIN_TAIL_NS);
    }
    while now_ns() < due_ns {
        std::hint::spin_loop();
    }
}

pub fn sched_fifo(prio: i32) -> i32 {
    #[cfg(unix)]
    unsafe {
        let mut param: libc::sched_param = std::mem::zeroed();
        param.sched_priority = prio;
        libc::sched_setscheduler(0, libc::SCHED_FIFO, &param)
    }
    #[cfg(not(unix))]
    {
        let _ = prio;
        -1
    }
}

pub fn chmod_644(path: &str) {
    #[cfg(unix)]
    {
        if let Ok(c) = std::ffi::CString::new(path) {
            unsafe {
                libc::chmod(c.as_ptr(), 0o644);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Single-instance guard at the daemon lock path. A second daemon must fail with
/// exit code 3 (C v5.3 contract), so the lock is held for the process lifetime.
pub struct InstanceLock {
    #[cfg(unix)]
    fd: libc::c_int,
    #[cfg(windows)]
    handle: isize,
}

pub const LOCK_BUSY: i32 = 3;

pub fn acquire_instance_lock(path: &str) -> Result<InstanceLock, io::Error> {
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "lock path NUL"))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC, 0o644) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }
        Ok(InstanceLock { fd })
    }
    #[cfg(windows)]
    {
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const OPEN_ALWAYS: u32 = 4;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
        const INVALID_HANDLE_VALUE: isize = -1;
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateFileW(
                name: *const u16,
                access: u32,
                share: u32,
                security: *const std::ffi::c_void,
                disposition: u32,
                flags: u32,
                template: isize,
            ) -> isize;
        }
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0, // no sharing: the second opener gets ERROR_SHARING_VIOLATION
                std::ptr::null(),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(InstanceLock { handle })
        }
    }
}

/// Resolve the egress interface address for a multicast/unicast destination
/// (used for SDP origin, the source filter address and IP_MULTICAST_IF).
pub fn detect_local_ip(dest: Ipv4Addr, port: u16) -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect((dest, port)).ok()?;
    match sock.local_addr().ok()? {
        std::net::SocketAddr::V4(a) => {
            if a.ip().is_unspecified() || a.ip().is_loopback() {
                None
            } else {
                Some(*a.ip())
            }
        }
        _ => None,
    }
}

pub fn open_sender_socket(local_ip: Ipv4Addr) -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind((local_ip, 0))?;
    sock.set_nonblocking(true)?;
    #[cfg(unix)]
    unsafe {
        use std::os::unix::io::AsRawFd;
        let one: libc::c_int = 1;
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let sndbuf: libc::c_int = 4 << 20;
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &sndbuf as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    Ok(sock)
}

#[cfg(unix)]
fn sockaddr_in(ip: Ipv4Addr) -> libc::sockaddr_in {
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = 0;
    addr.sin_addr.s_addr = u32::from_ne_bytes(ip.octets());
    addr
}

/// Select the outgoing interface for multicast (IP_MULTICAST_IF). Windows has no
/// stable std API for this; the sender also binds to the local address which is
/// enough for the host loopback tests, so the call is a no-op there.
pub fn set_multicast_interface(sock: &UdpSocket, ip: Ipv4Addr) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let addr = sockaddr_in(ip);
        let rc = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::IPPROTO_IP,
                libc::IP_MULTICAST_IF,
                &addr as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (sock, ip);
        Ok(())
    }
}

pub fn set_multicast_ttl(sock: &UdpSocket, ttl: u32) {
    let _ = sock.set_multicast_ttl_v4(ttl);
}

pub fn set_multicast_loop(sock: &UdpSocket, on: bool) {
    let _ = sock.set_multicast_loop_v4(on);
}

pub fn parse_ipv4(text: &str) -> Option<Ipv4Addr> {
    text.trim().parse::<Ipv4Addr>().ok()
}

pub fn destination(text: &str, port: u16) -> Option<SocketAddrV4> {
    parse_ipv4(text).map(|ip| SocketAddrV4::new(ip, port))
}
