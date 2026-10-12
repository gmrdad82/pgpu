use std::ffi::{CStr, OsStr};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static WRITES: AtomicU64 = AtomicU64::new(0);

fn live_runtime() -> PathBuf {
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

fn runtime() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(live_runtime)
}

pub(crate) fn user_home() -> PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| passwd_home().unwrap_or_else(runtime))
        .clone()
}

fn passwd_home() -> Option<PathBuf> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    if code != 0 || found.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    let dir = unsafe { CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(OsStr::from_bytes(dir.to_bytes()))).filter(|dir| dir.is_absolute())
}

fn xdg(var: &str, below_home: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|dir| !dir.is_empty() && runtime() != live_runtime())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(below_home))
}

const HOME_FOLDERS: [&str; 10] = [
    "tickets", "served", "turns", "stopped", "desk", "play", "lent", "plays", "reclaim", "offered",
];

pub(crate) fn home() -> PathBuf {
    runtime().join("pito-gpu-queue")
}

pub(crate) fn make_home() -> PathBuf {
    let dir = home();
    make_folders(&dir);
    dir
}

fn make_folders(dir: &Path) {
    for folder in HOME_FOLDERS {
        let _ = fs::create_dir_all(dir.join(folder));
    }
}

pub(crate) fn config_root() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub(crate) fn config() -> PathBuf {
    config_root().join("pgpu")
}

pub(crate) fn state() -> PathBuf {
    let dir = xdg("XDG_STATE_HOME", ".local/state").join("pito-gpu-queue");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub(crate) fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub(crate) fn millis() -> u128 {
    now() / 1_000_000
}

pub(crate) fn turn_file(dir: &Path, pid: u32, suffix: &str) -> PathBuf {
    dir.join("turns").join(format!("{pid}{suffix}"))
}

const SMALL: usize = 4096;

pub(crate) fn read_small(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; SMALL];
    let n = file.read(&mut buf)?;
    if n == SMALL {
        return fs::read_to_string(path);
    }
    String::from_utf8(buf[..n].to_vec())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub(crate) fn stat_start(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

pub(crate) fn stat_state(stat: &str) -> Option<char> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().next()?.chars().next()
}

pub(crate) fn stat_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

pub(crate) fn clock_ticks() -> u64 {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks as u64 } else { 100 }
}

pub(crate) fn boot_secs(proc_root: &Path) -> Option<u64> {
    fs::read_to_string(proc_root.join("stat"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())
}

pub(crate) fn start_ms(ticks: u64, per_second: u64, btime_secs: u64) -> u128 {
    u128::from(btime_secs) * 1000 + u128::from(ticks) * 1000 / u128::from(per_second.max(1))
}

pub(crate) fn proc_stat(proc_root: &Path, pid: u32) -> Option<String> {
    read_small(&proc_root.join(pid.to_string()).join("stat")).ok()
}

pub(crate) fn start_time(pid: u32) -> Option<u64> {
    stat_start(&proc_stat(Path::new("/proc"), pid)?)
}

pub(crate) fn alive_as(pid: u32, start: u64) -> bool {
    start_time(pid) == Some(start)
}

pub(crate) fn write_atomic(path: &Path, body: &str) -> io::Result<()> {
    write_atomic_bytes(path, body.as_bytes())
}

pub(crate) fn write_atomic_bytes(path: &Path, body: &[u8]) -> io::Result<()> {
    write_dated(path, body, None)
}

pub(crate) fn rewrite_keeping_time(path: &Path, body: &str) -> io::Result<()> {
    let modified = fs::metadata(path)?.modified()?;
    write_dated(path, body.as_bytes(), Some(modified))
}

fn write_dated(path: &Path, body: &[u8], modified: Option<SystemTime>) -> io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = path.with_file_name(name);
    let written = fs::write(&temporary, body)
        .and_then(|_| match modified {
            Some(time) => File::options()
                .write(true)
                .open(&temporary)?
                .set_modified(time),
            None => Ok(()),
        })
        .and_then(|_| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

fn line_to(fd: RawFd, line: &str) {
    let mut ready = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    if unsafe { libc::poll(&mut ready, 1, 0) } != 1 || ready.revents & libc::POLLOUT == 0 {
        return;
    }
    let mut out = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let _ = out.write_all(format!("{line}\n").as_bytes());
}

pub(crate) fn say(line: &str) {
    line_to(libc::STDERR_FILENO, line);
}

pub(crate) fn say_out(line: &str) {
    line_to(libc::STDOUT_FILENO, line);
}

pub(crate) fn stamp(path: &Path) -> Option<u128> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::{
        alive_as, boot_secs, config_root, line_to, make_folders, passwd_home, read_small,
        rewrite_keeping_time, start_ms, start_time, stat_ppid, stat_start, stat_state,
        write_atomic,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn pipe() -> (i32, i32) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    fn written_in_time(fd: i32) -> bool {
        let (done, seen) = mpsc::channel();
        thread::spawn(move || {
            line_to(fd, "pgpu: waiting");
            let _ = done.send(());
        });
        seen.recv_timeout(Duration::from_secs(5)).is_ok()
    }

    #[test]
    fn a_line_to_a_full_or_closed_pipe_neither_blocks_nor_panics() {
        let (read, write) = pipe();
        assert!(written_in_time(write));
        let mut got = [0u8; 64];
        let n = unsafe { libc::read(read, got.as_mut_ptr().cast(), got.len()) };
        assert_eq!(&got[..n as usize], b"pgpu: waiting\n");
        let flags = unsafe { libc::fcntl(write, libc::F_GETFL) };
        unsafe { libc::fcntl(write, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        let block = [b'x'; 4096];
        while unsafe { libc::write(write, block.as_ptr().cast(), block.len()) } > 0 {}
        unsafe { libc::fcntl(write, libc::F_SETFL, flags) };
        assert!(written_in_time(write), "a full pipe blocked the line");
        unsafe { libc::close(read) };
        assert!(written_in_time(write), "a closed pipe stopped the line");
        unsafe { libc::close(write) };
    }

    #[test]
    fn the_state_letter_follows_the_name() {
        assert_eq!(stat_state("4242 (a) b (c)) T 1 4242"), Some('T'));
        assert_eq!(stat_state("7 (sh) t 1"), Some('t'));
        assert_eq!(stat_state("7 (sh) S 1"), Some('S'));
        assert_eq!(stat_state("7 (sh)"), None);
        assert_eq!(stat_state("no paren"), None);
    }

    #[test]
    fn the_start_time_is_field_22_even_with_odd_names() {
        let stat =
            "4242 (a) b (c)) S 1 4242 4242 0 -1 4194304 100 0 0 0 5 6 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(stat_start(stat), Some(987654));
        assert_eq!(stat_start("4242 (x) S 1"), None);
        assert_eq!(stat_start("no paren"), None);
        assert_eq!(stat_ppid(stat), Some(1));
        assert_eq!(stat_ppid("4242 (x) S"), None);
        assert_eq!(stat_ppid("no paren"), None);
    }

    #[test]
    fn a_start_in_ticks_becomes_epoch_milliseconds() {
        assert_eq!(start_ms(0, 100, 1_700_000_000), 1_700_000_000_000);
        assert_eq!(start_ms(12_345, 100, 1_700_000_000), 1_700_000_123_450);
        assert_eq!(start_ms(7, 250, 10), 10_028);
        assert_eq!(start_ms(5, 0, 10), 15_000);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join("test-paths-btime");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(boot_secs(&dir), None);
        fs::write(
            dir.join("stat"),
            "cpu  1 2 3\nbtime 1700000000\nprocesses 9\n",
        )
        .unwrap();
        assert_eq!(boot_secs(&dir), Some(1_700_000_000));
    }

    #[test]
    fn a_pid_with_another_start_time_is_not_the_same_process() {
        let me = std::process::id();
        let start = start_time(me).unwrap();
        assert!(alive_as(me, start));
        assert!(!alive_as(me, start + 1));
    }

    #[test]
    fn a_small_proc_file_is_read_whole_and_a_longer_one_too() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-read-small");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let short = dir.join("comm");
        fs::write(&short, "skewgame\n").unwrap();
        assert_eq!(read_small(&short).unwrap(), "skewgame\n");
        let long = dir.join("stat");
        let body = "x".repeat(5000);
        fs::write(&long, &body).unwrap();
        assert_eq!(read_small(&long).unwrap(), body);
        assert!(read_small(&dir.join("missing")).is_err());
    }

    #[test]
    fn a_job_on_the_live_queue_reads_the_users_own_config_whatever_its_home() {
        if let Some(expect) = std::env::var_os("PGPU_HOME_CHILD") {
            assert_eq!(config_root(), PathBuf::from(expect));
            return;
        }
        let home = passwd_home().unwrap();
        let tape = "/nonexistent/tape-home";
        let sandbox = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-home-run");
        for (runtime, expect) in [
            (None, home.join(".config")),
            (Some(sandbox.as_path()), PathBuf::from(tape).join(".config")),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "paths::tests::a_job_on_the_live_queue_reads_the_users_own_config_whatever_its_home",
                ])
                .env("PGPU_HOME_CHILD", &expect)
                .env("HOME", tape)
                .env("XDG_CONFIG_HOME", PathBuf::from(tape).join(".config"));
            match runtime {
                Some(dir) => child.env("XDG_RUNTIME_DIR", dir),
                None => child.env_remove("XDG_RUNTIME_DIR"),
            };
            let out = child.output().unwrap();
            assert!(
                out.status.success(),
                "{runtime:?}: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }

    #[test]
    fn the_queue_folders_are_made_only_on_asking() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-folders");
        let _ = fs::remove_dir_all(&dir);
        assert!(!dir.exists());
        make_folders(&dir);
        for folder in [
            "tickets", "served", "turns", "stopped", "desk", "play", "lent", "plays", "reclaim",
            "offered",
        ] {
            assert!(dir.join(folder).is_dir(), "{folder}");
        }
        make_folders(&dir);
    }

    #[test]
    fn an_atomic_write_leaves_no_temporary() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-atomic");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("running");
        write_atomic(&path, "a\n").unwrap();
        write_atomic(&path, "b\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(write_atomic(&dir.join("missing/x"), "c").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn a_rewrite_keeping_time_changes_the_body_and_not_the_mtime() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-keep-time");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ticket");
        write_atomic(&path, "a\n").unwrap();
        let then = std::time::SystemTime::now() - Duration::from_secs(90);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(then)
            .unwrap();
        rewrite_keeping_time(&path, "b\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), then);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(rewrite_keeping_time(&dir.join("missing"), "c").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }
}
