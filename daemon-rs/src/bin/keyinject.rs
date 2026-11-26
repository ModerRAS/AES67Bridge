//! aes67-keyinject: emit KEY_VOLUMEUP/DOWN/MUTE through /dev/uinput.
//!
//! Test helper for the daemon's passive evdev reader: creates a virtual
//! keyboard, waits for the daemon's device scan to pick it up, emits the key,
//! then destroys the device.
//!
//! Usage: aes67-keyinject [--code 115|114|113] [--times N] [--wait S] [--hold-ms N] [--gap-ms N]

#[cfg(any(target_os = "linux", target_os = "android"))]
fn main() {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;
    use std::thread;
    use std::time::Duration;

    const UI_DEV_CREATE: u64 = 0x5501;
    const UI_DEV_DESTROY: u64 = 0x5502;
    const UI_SET_EVBIT: u64 = 0x4004_5564;
    const UI_SET_KEYBIT: u64 = 0x4004_5565;
    const EV_SYN: u16 = 0;
    const EV_KEY: u16 = 1;
    const SYN_REPORT: u16 = 0;

    #[repr(C)]
    struct InputId {
        bustype: u16,
        vendor: u16,
        product: u16,
        version: u16,
    }

    #[repr(C)]
    struct UinputUserDev {
        name: [u8; 80],
        id: InputId,
        ff_effects_max: u32,
        absmax: [i32; 64],
        absmin: [i32; 64],
        absfuzz: [i32; 64],
        absflat: [i32; 64],
    }

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

    fn emit(file: &mut std::fs::File, type_: u16, code: u16, value: i32) {
        use std::io::Write;
        let event = InputEvent {
            sec: 0,
            usec: 0,
            type_,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &event as *const InputEvent as *const u8,
                std::mem::size_of::<InputEvent>(),
            )
        };
        let _ = file.write_all(bytes);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut code: u16 = 115;
    let mut times = 1usize;
    let mut wait_s = 3f64;
    let mut hold_ms = 60u64;
    let mut gap_ms = 250u64;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1);
        match args[i].as_str() {
            "--code" => {
                code = value.and_then(|v| v.parse().ok()).unwrap_or(115);
                i += 2;
            }
            "--times" => {
                times = value.and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
                i += 2;
            }
            "--wait" => {
                wait_s = value.and_then(|v| v.parse().ok()).unwrap_or(3.0);
                i += 2;
            }
            "--hold-ms" => {
                hold_ms = value.and_then(|v| v.parse().ok()).unwrap_or(60);
                i += 2;
            }
            "--gap-ms" => {
                gap_ms = value.and_then(|v| v.parse().ok()).unwrap_or(250);
                i += 2;
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
    }

    let mut file = match OpenOptions::new().write(true).open("/dev/uinput") {
        Ok(file) => file,
        Err(err) => {
            eprintln!("open /dev/uinput: {err}");
            std::process::exit(1);
        }
    };
    let fd = file.as_raw_fd();
    unsafe {
        for ev in [EV_KEY] {
            if libc::ioctl(fd, UI_SET_EVBIT as _, ev as libc::c_int) != 0 {
                eprintln!("UI_SET_EVBIT({ev}): {}", std::io::Error::last_os_error());
                std::process::exit(1);
            }
        }
        for key in [113u16, 114, 115] {
            if libc::ioctl(fd, UI_SET_KEYBIT as _, key as libc::c_int) != 0 {
                eprintln!("UI_SET_KEYBIT({key}): {}", std::io::Error::last_os_error());
                std::process::exit(1);
            }
        }
        let mut dev = UinputUserDev {
            name: [0; 80],
            id: InputId {
                bustype: 0x03,
                vendor: 0x1209,
                product: 0x0001,
                version: 1,
            },
            ff_effects_max: 0,
            absmax: [0; 64],
            absmin: [0; 64],
            absfuzz: [0; 64],
            absflat: [0; 64],
        };
        let name = b"aes67-keyinject";
        dev.name[..name.len()].copy_from_slice(name);
        let bytes = std::slice::from_raw_parts(
            &dev as *const UinputUserDev as *const u8,
            std::mem::size_of::<UinputUserDev>(),
        );
        use std::io::Write;
        if file.write_all(bytes).is_err() {
            eprintln!("write uinput_user_dev failed");
            std::process::exit(1);
        }
        if libc::ioctl(fd, UI_DEV_CREATE as _) != 0 {
            eprintln!("UI_DEV_CREATE: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
    }
    println!("virtual keyboard created; waiting {wait_s:.1}s for the daemon scan");
    thread::sleep(Duration::from_secs_f64(wait_s));
    for _ in 0..times {
        emit(&mut file, EV_KEY, code, 1);
        emit(&mut file, EV_SYN, SYN_REPORT, 0);
        thread::sleep(Duration::from_millis(hold_ms));
        emit(&mut file, EV_KEY, code, 0);
        emit(&mut file, EV_SYN, SYN_REPORT, 0);
        thread::sleep(Duration::from_millis(gap_ms));
    }
    unsafe {
        libc::ioctl(fd, UI_DEV_DESTROY as _);
    }
    println!("done code={code} times={times}");
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn main() {
    eprintln!("aes67-keyinject is linux/android-only");
    std::process::exit(2);
}