//! PCM input source.
//!
//! Production input is the HAL tee FIFO `/data/haltee/live.pcm`, opened
//! read-only non-blocking and drained eagerly (C v5.3 read strategy). On hosts
//! without FIFOs the same `--fifo` path may be a regular file that a feeder
//! appends to; it is tailed instead, which keeps the synthetic host test close
//! to the device data flow.

use std::fs::File;
use std::io;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    Readable,
    EndOfStream,
    Timeout,
}

pub enum Source {
    #[cfg(unix)]
    Fifo(FifoSource),
    File(FileSource),
}

pub fn open(path: &str) -> io::Result<Source> {
    // Check the path type BEFORE opening: a blocking open() of a FIFO with no
    // writer blocks until the HAL proxy appears, which would freeze startup and
    // the status heartbeat (observed on device as D-state "fifo_open"). stat()
    // is non-blocking, so open FIFOs with O_NONBLOCK directly.
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path NUL"))?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::stat(c.as_ptr(), &mut stat) } == 0
            && (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFIFO as u32
        {
            return Ok(Source::Fifo(FifoSource::open(path)?));
        }
    }
    let mut file = File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file.metadata()?.file_type().is_fifo() {
            drop(file);
            return Ok(Source::Fifo(FifoSource::open(path)?));
        }
    }
    // Regular file: start at the end of the current content, then tail appends.
    let position = file.seek(SeekFrom::End(0))?;
    Ok(Source::File(FileSource { file, path: path.to_string(), position }))
}

impl Source {
    pub fn available(&self) -> usize {
        match self {
            #[cfg(unix)]
            Source::Fifo(fifo) => fifo.available(),
            Source::File(file) => file.available(),
        }
    }

    /// Eagerly read whatever is available, appending to `out` (up to 64 KiB).
    /// Returns the number of bytes read and whether the writer went away.
    pub fn read_available(&mut self, out: &mut Vec<u8>) -> io::Result<(usize, bool)> {
        match self {
            #[cfg(unix)]
            Source::Fifo(fifo) => fifo.read_available(out),
            Source::File(file) => file.read_available(out),
        }
    }

    pub fn wait(&mut self, timeout_ms: i32) -> Wait {
        match self {
            #[cfg(unix)]
            Source::Fifo(fifo) => fifo.wait(timeout_ms),
            Source::File(file) => file.wait(timeout_ms),
        }
    }
}

#[cfg(unix)]
pub struct FifoSource {
    fd: libc::c_int,
}

#[cfg(unix)]
impl FifoSource {
    fn open(path: &str) -> io::Result<Self> {
        let c = std::ffi::CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fifo path NUL"))?;
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    fn available(&self) -> usize {
        let mut bytes: libc::c_int = 0;
        let rc = unsafe { libc::ioctl(self.fd, libc::FIONREAD, &mut bytes) };
        if rc < 0 || bytes <= 0 {
            0
        } else {
            bytes as usize
        }
    }

    fn read_available(&mut self, out: &mut Vec<u8>) -> io::Result<(usize, bool)> {
        const BLOCK: usize = 65_536;
        let mut total = 0usize;
        let mut buffer = [0_u8; BLOCK];
        loop {
            let count = unsafe { libc::read(self.fd, buffer.as_mut_ptr().cast(), BLOCK) };
            if count > 0 {
                out.extend_from_slice(&buffer[..count as usize]);
                total += count as usize;
                if total >= BLOCK {
                    break;
                }
            } else if count == 0 {
                // Writer closed: EOF for this stream; the reader stays open.
                return Ok((total, true));
            } else {
                let err = io::Error::last_os_error();
                match err.kind() {
                    io::ErrorKind::WouldBlock => break,
                    io::ErrorKind::Interrupted => continue,
                    _ => return Err(err),
                }
            }
        }
        Ok((total, false))
    }

    fn wait(&mut self, timeout_ms: i32) -> Wait {
        let mut pollfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc < 0 {
            return Wait::Timeout;
        }
        if rc == 0 {
            return Wait::Timeout;
        }
        if pollfd.revents & libc::POLLIN != 0 {
            Wait::Readable
        } else if pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            Wait::EndOfStream
        } else {
            Wait::Timeout
        }
    }
}

pub struct FileSource {
    file: File,
    path: String,
    position: u64,
}

impl FileSource {
    fn available(&self) -> usize {
        self.file
            .metadata()
            .map(|meta| meta.len().saturating_sub(self.position) as usize)
            .unwrap_or(0)
    }

    fn read_available(&mut self, out: &mut Vec<u8>) -> io::Result<(usize, bool)> {
        const BLOCK: usize = 65_536;
        let mut total = 0usize;
        let mut buffer = [0_u8; BLOCK];
        loop {
            let count = self.file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            out.extend_from_slice(&buffer[..count]);
            total += count;
            self.position += count as u64;
            if total >= BLOCK {
                break;
            }
        }
        Ok((total, false))
    }

    fn wait(&mut self, timeout_ms: i32) -> Wait {
        let deadline = crate::platform::now_ns()
            + i64::from(timeout_ms.max(1)) * 1_000_000;
        while crate::platform::now_ns() < deadline {
            if self.available() > 0 {
                return Wait::Readable;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        if self.available() > 0 {
            Wait::Readable
        } else {
            let _ = &self.path;
            Wait::Timeout
        }
    }
}
