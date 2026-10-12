use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use crate::paths::turn_file;

pub(crate) const WAKE: &str = ".wake";

pub(crate) const BACK: &str = ".back";

pub(crate) const RING: u8 = b'w';

pub(crate) const LOOK: u8 = b'l';

static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

pub(crate) struct Bell {
    file: File,
}

impl Bell {
    pub(crate) fn make(path: &Path) -> Option<Self> {
        let _ = fs::remove_file(path);
        let name = CString::new(path.as_os_str().as_bytes()).ok()?;
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return None;
        }
        Self::open(path)
    }

    pub(crate) fn open(path: &Path) -> Option<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        file.metadata()
            .is_ok_and(|meta| {
                use std::os::unix::fs::FileTypeExt;
                meta.file_type().is_fifo()
            })
            .then_some(Self { file })
    }

    pub(crate) fn on_signal(&self) {
        SIGNAL_FD.store(self.file.as_raw_fd(), Ordering::SeqCst);
    }

    pub(crate) fn wait(&self, also: Option<RawFd>, timeout: Duration) -> Rung {
        let mut fds = [
            libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: also.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        unsafe {
            libc::poll(fds.as_mut_ptr(), 2, ms);
        }
        let mut rung = Rung::default();
        if fds[0].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 256];
            let mut file = &self.file;
            while let Ok(n) = file.read(&mut buf) {
                if n == 0 {
                    break;
                }
                rung.rung = true;
                rung.look |= buf[..n].contains(&LOOK);
                if n < buf.len() {
                    break;
                }
            }
        }
        rung
    }
}

impl Drop for Bell {
    fn drop(&mut self) {
        let _ = SIGNAL_FD.compare_exchange(
            self.file.as_raw_fd(),
            -1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rung {
    pub(crate) rung: bool,
    pub(crate) look: bool,
}

pub(crate) fn signalled() {
    let fd = SIGNAL_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            libc::write(fd, [RING].as_ptr().cast(), 1);
        }
    }
}

pub(crate) fn ring(path: &Path, what: u8) {
    if let Ok(mut file) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        let _ = file.write(&[what]);
    }
}

pub(crate) fn ring_wrapper(dir: &Path, pid: u32, what: u8) {
    ring(&turn_file(dir, pid, WAKE), what);
}

pub(crate) fn wake_path(dir: &Path, pid: u32) -> PathBuf {
    turn_file(dir, pid, WAKE)
}

pub(crate) fn back_path(dir: &Path, pid: u32) -> PathBuf {
    turn_file(dir, pid, BACK)
}

pub(crate) struct Exit {
    fd: OwnedFd,
}

impl Exit {
    pub(crate) fn of(pid: u32) -> Option<Self> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        let fd = RawFd::try_from(fd).ok().filter(|fd| *fd >= 0)?;
        Some(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::{Bell, Exit, LOOK, RING, ring};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-wake-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_ring_wakes_the_bell_at_once_and_is_drained() {
        let dir = test_dir("ring");
        let path = dir.join("1.wake");
        let bell = Bell::make(&path).unwrap();
        let t = Instant::now();
        assert!(!bell.wait(None, Duration::from_millis(30)).rung);
        assert!(t.elapsed() >= Duration::from_millis(25));
        ring(&path, RING);
        ring(&path, LOOK);
        let t = Instant::now();
        let rung = bell.wait(None, Duration::from_secs(5));
        assert!(rung.rung && rung.look);
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(!bell.wait(None, Duration::ZERO).rung);
    }

    #[test]
    fn ringing_a_bell_nobody_holds_does_nothing() {
        let dir = test_dir("nobody");
        let path = dir.join("2.back");
        drop(Bell::make(&path).unwrap());
        ring(&path, RING);
        ring(&dir.join("missing.wake"), RING);
        let bell = Bell::open(&path).unwrap();
        assert!(!bell.wait(None, Duration::ZERO).rung);
    }

    #[test]
    fn a_plain_file_is_no_bell() {
        let dir = test_dir("plain");
        let path = dir.join("3.wake");
        fs::write(&path, "x").unwrap();
        assert!(Bell::open(&path).is_none());
    }

    #[test]
    fn a_child_exit_wakes_the_bell() {
        let dir = test_dir("exit");
        let bell = Bell::make(&dir.join("4.wake")).unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let exit = Exit::of(child.id()).unwrap();
        let t = Instant::now();
        bell.wait(Some(exit.fd()), Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(child.try_wait().unwrap().is_some());
    }
}
