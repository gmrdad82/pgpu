use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::sleep;
use std::time::{Duration, SystemTime};

use crate::paths::{alive_as, millis, proc_stat, read_small, stamp, start_time, stat_ppid};
use crate::queue::{holder_of, tickets};

const SERVED_KEEP: Duration = Duration::from_secs(7 * 86_400);

const TEMPORARY_KEEP: Duration = Duration::from_secs(60);

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

const CELL_PREFIX: &str = "pgpu-";

const CELL_LINE: &str = "cgroup ";

const SIGSTOP_MARK: &str = "sigstop";

const EMPTYING_ROUNDS: usize = 500;

pub(crate) fn tree(root: u32) -> Vec<u32> {
    let mut parents: Vec<(u32, u32)> = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
                continue;
            };
            let stat = read_small(&entry.path().join("stat")).unwrap_or_default();
            if let Some(ppid) = stat_ppid(&stat) {
                parents.push((pid, ppid));
            }
        }
    }
    let mut out = vec![root];
    let mut at = 0;
    while at < out.len() {
        let parent = out[at];
        out.extend(
            parents
                .iter()
                .filter(|(_, p)| *p == parent)
                .map(|(c, _)| *c),
        );
        at += 1;
    }
    out
}

pub(crate) fn kin(proc_root: &Path, seeds: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for pid in seeds {
        if !out.contains(pid) {
            out.push(*pid);
        }
    }
    let mut at = 0;
    while at < out.len() {
        let tasks = proc_root.join(out[at].to_string()).join("task");
        at += 1;
        let Ok(entries) = fs::read_dir(&tasks) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(body) = fs::read_to_string(entry.path().join("children")) else {
                continue;
            };
            for pid in body.split_whitespace().filter_map(|id| id.parse().ok()) {
                if !out.contains(&pid) {
                    out.push(pid);
                }
            }
        }
    }
    out
}

static ENDING: AtomicI32 = AtomicI32::new(0);

extern "C" fn ending(sig: libc::c_int) {
    ENDING.store(sig, Ordering::SeqCst);
    crate::wake::signalled();
}

pub(crate) fn ended() -> Option<i32> {
    Some(ENDING.load(Ordering::SeqCst)).filter(|sig| *sig != 0)
}

pub(crate) fn catch_endings() {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            libc::signal(sig, ending as *const () as libc::sighandler_t);
        }
    }
}

pub(crate) type Proc = (u32, u64);

pub(crate) fn with_starts(pids: &[u32]) -> Vec<Proc> {
    pids.iter()
        .filter_map(|pid| start_time(*pid).map(|start| (*pid, start)))
        .collect()
}

pub(crate) fn signal(pids: &[Proc], stop: bool) {
    send(pids, if stop { libc::SIGSTOP } else { libc::SIGCONT });
}

fn cpu_ticks(stat: &str) -> Option<u64> {
    let mut rest = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .skip(11);
    let user: u64 = rest.next()?.parse().ok()?;
    let system: u64 = rest.next()?.parse().ok()?;
    Some(user + system)
}

pub(crate) fn cpu_of(pids: &[u32]) -> Duration {
    let ticks: u64 = pids
        .iter()
        .filter_map(|pid| {
            proc_stat(Path::new("/proc"), *pid)
                .as_deref()
                .and_then(cpu_ticks)
        })
        .sum();
    Duration::from_secs_f64(ticks as f64 / crate::paths::clock_ticks().max(1) as f64)
}

pub(crate) fn send(pids: &[Proc], sig: i32) {
    for (pid, start) in pids {
        if alive_as(*pid, *start) {
            unsafe {
                libc::kill(*pid as libc::pid_t, sig);
            }
        }
    }
}

pub(crate) fn parked_body(pids: &[Proc], wrapper_start: u64) -> String {
    let list: Vec<String> = pids
        .iter()
        .map(|(pid, start)| format!("{pid}:{start}"))
        .collect();
    format!("{}\n{wrapper_start}\n", list.join(" "))
}

pub(crate) fn parse_parked(body: &str) -> (Vec<Proc>, Option<u64>) {
    let mut lines = body.lines();
    let pids = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|token| {
            let (pid, start) = token.split_once(':')?;
            Some((pid.parse().ok()?, start.parse().ok()?))
        })
        .collect();
    let wrapper = lines.next().and_then(|line| line.trim().parse().ok());
    (pids, wrapper)
}

pub(crate) fn sigstop_body(pids: &[Proc], wrapper_start: u64) -> String {
    format!("{}{SIGSTOP_MARK}\n", parked_body(pids, wrapper_start))
}

pub(crate) fn cell_body(cell: &Cell, wrapper_start: u64) -> String {
    format!("{CELL_LINE}{}\n{wrapper_start}\n", cell.path.display())
}

pub(crate) fn parse_cell(body: &str) -> Option<PathBuf> {
    body.lines()
        .next()?
        .strip_prefix(CELL_LINE)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn ids(path: &Path) -> Vec<u32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|id| id.parse().ok())
        .collect()
}

fn cgroup_line(text: &str) -> Option<PathBuf> {
    let path = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    Some(Path::new(CGROUP_ROOT).join(path.trim_start_matches('/')))
}

fn cgroup_of(pid: &str) -> Option<PathBuf> {
    cgroup_line(&fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
}

fn put(path: &Path, text: &str) -> bool {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .is_ok()
}

fn inside(path: &Path) -> bool {
    path.starts_with(CGROUP_ROOT)
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn ours(path: &Path) -> bool {
    inside(path)
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(CELL_PREFIX))
}

pub(crate) struct Cell {
    path: PathBuf,
    pub(crate) ceiling: Option<u64>,
}

fn offers_memory(own: &Path) -> bool {
    fs::read_to_string(own.join("cgroup.controllers"))
        .is_ok_and(|names| names.split_whitespace().any(|name| name == "memory"))
        && own.parent().is_some_and(|parent| {
            OpenOptions::new()
                .write(true)
                .open(parent.join("cgroup.procs"))
                .is_ok()
        })
}

impl Cell {
    pub(crate) fn make(me: u32, memory: u64) -> io::Result<Self> {
        let own = cgroup_of("self").ok_or_else(|| io::Error::other("no cgroup v2 here"))?;
        let name = format!("{CELL_PREFIX}{me}");
        let beside = own
            .parent()
            .filter(|_| offers_memory(&own))
            .map(|parent| parent.join(&name))
            .filter(|path| fs::create_dir(path).is_ok());
        let path = match beside {
            Some(path) => path,
            None => {
                let path = own.join(&name);
                fs::create_dir(&path)?;
                path
            }
        };
        let mut cell = Self {
            path,
            ceiling: None,
        };
        if !cell.path.join("cgroup.freeze").exists() {
            cell.close();
            return Err(io::Error::other("its cgroup has no freezer"));
        }
        cell.ceiling = cell.limit(memory).then_some(memory);
        Ok(cell)
    }

    fn limit(&self, bytes: u64) -> bool {
        let swap = self.path.join("memory.swap.max");
        self.path.join("memory.max").exists()
            && (!swap.exists() || put(&swap, "0"))
            && put(&self.path.join("memory.oom.group"), "1")
            && put(
                &self.path.join("memory.high"),
                &crate::budget::high(bytes).to_string(),
            )
            && put(&self.path.join("memory.max"), &bytes.to_string())
    }

    pub(crate) fn at(path: PathBuf) -> Option<Self> {
        ours(&path).then_some(Self {
            path,
            ceiling: None,
        })
    }

    pub(crate) fn dir(&self) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(&self.path)
    }

    pub(crate) fn holds(&self, pid: u32) -> bool {
        cgroup_of(&pid.to_string()).is_none_or(|path| path == self.path)
    }

    pub(crate) fn freeze(&self, on: bool) -> bool {
        put(&self.path.join("cgroup.freeze"), if on { "1" } else { "0" })
    }

    pub(crate) fn pids(&self) -> Vec<u32> {
        ids(&self.path.join("cgroup.procs"))
    }

    pub(crate) fn cpu(&self) -> Option<Duration> {
        fs::read_to_string(self.path.join("cpu.stat"))
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("usage_usec "))
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_micros)
    }

    pub(crate) fn out_of_memory(&self) -> bool {
        fs::read_to_string(self.path.join("memory.events")).is_ok_and(|body| {
            body.lines().any(|line| {
                line.strip_prefix("oom_kill ")
                    .or_else(|| line.strip_prefix("oom_group_kill "))
                    .is_some_and(|count| count.trim() != "0")
            })
        })
    }

    pub(crate) fn peak(&self) -> Option<u64> {
        fs::read_to_string(self.path.join("memory.peak"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub(crate) fn kill(&self) -> bool {
        put(&self.path.join("cgroup.kill"), "1")
    }

    pub(crate) fn close(&self) -> bool {
        self.freeze(false);
        if !self.pids().is_empty() {
            self.kill();
        }
        for _ in 0..EMPTYING_ROUNDS {
            if fs::remove_dir(&self.path).is_ok() || !self.path.exists() {
                return true;
            }
            sleep(Duration::from_millis(10));
        }
        false
    }
}

fn turn_owner(name: &str) -> Option<u32> {
    let pid = name
        .strip_suffix(".last")
        .or_else(|| name.strip_suffix(".served"))
        .or_else(|| name.strip_suffix(".pace"))
        .or_else(|| name.strip_suffix(".lock"))
        .or_else(|| name.strip_suffix(".rest"))
        .or_else(|| name.strip_suffix(".frozen"))
        .or_else(|| name.strip_suffix(".ran"))
        .or_else(|| name.strip_suffix(crate::wake::WAKE))
        .or_else(|| name.strip_suffix(crate::wake::BACK))
        .unwrap_or(name);
    pid.parse().ok()
}

fn older(path: &Path, now: SystemTime, keep: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > keep))
}

fn stale(path: &Path, now: SystemTime) -> bool {
    older(path, now, SERVED_KEEP)
}

fn old_temporary(name: &str, path: &Path, now: SystemTime) -> bool {
    name.ends_with(".tmp") && older(path, now, TEMPORARY_KEEP)
}

fn drop_old_temporaries(folder: &Path) {
    let now = SystemTime::now();
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if old_temporary(&entry.file_name().to_string_lossy(), &path, now) {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn turned_this_second(dir: &Path) -> bool {
    let Some(pid) = std::env::var("GPU_QUEUE_PID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    else {
        return false;
    };
    same_second(
        stamp(&dir.join("turns").join(format!("{pid}.last"))),
        millis(),
    )
}

fn same_second(last_ms: Option<u128>, now_ms: u128) -> bool {
    last_ms.is_some_and(|last| last / 1000 == now_ms / 1000)
}

fn drop_turn_files(dir: &Path) {
    drop_turn_files_of(dir, || tickets(dir).iter().map(|t| t.pid).collect());
}

fn drop_turn_files_of(dir: &Path, live: impl FnOnce() -> Vec<u32>) {
    let Ok(marks) = fs::read_dir(dir.join("turns")) else {
        return;
    };
    let marks: Vec<(String, PathBuf)> = marks
        .flatten()
        .map(|mark| (mark.file_name().to_string_lossy().into_owned(), mark.path()))
        .collect();
    let live = live();
    let now = SystemTime::now();
    for (name, path) in marks {
        if turn_owner(&name).is_some_and(|pid| !live.contains(&pid))
            || old_temporary(&name, &path, now)
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn drop_dead_holder(dir: &Path) {
    for seat in 0..crate::seats::MOST {
        drop_dead_holder_at(dir, seat);
    }
}

fn drop_dead_holder_at(dir: &Path, seat: usize) {
    let running = crate::seats::running_path(dir, seat);
    let Ok(body) = fs::read_to_string(&running) else {
        return;
    };
    if holder_of(&body).is_some_and(|(pid, start)| alive_as(pid, start)) {
        return;
    }
    let Ok(lock) = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(crate::seats::lock_path(dir, seat))
    else {
        return;
    };
    if lock.try_lock().is_err() {
        return;
    }
    if fs::read_to_string(&running).is_ok_and(|again| again == body) {
        let _ = fs::remove_file(&running);
    }
    let _ = lock.unlock();
}

fn drop_dead_marks(dir: &Path, folder: &str) {
    let Ok(marks) = fs::read_dir(dir.join(folder)) else {
        return;
    };
    for mark in marks.flatten() {
        let Some(pid) = mark.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let start = fs::read_to_string(mark.path())
            .ok()
            .and_then(|body| body.lines().next().and_then(|v| v.trim().parse().ok()));
        if start.is_none_or(|start| !alive_as(pid, start)) {
            let _ = fs::remove_file(mark.path());
        }
    }
}

fn drop_stale_served(dir: &Path) {
    let now = SystemTime::now();
    if let Ok(names) = fs::read_dir(dir.join("served")) {
        for name in names.flatten() {
            if stale(&name.path(), now) {
                let _ = fs::remove_file(name.path());
            }
        }
    }
}

fn wake_orphans(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir.join("stopped")) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(wrapper) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let body = fs::read_to_string(entry.path()).unwrap_or_default();
        let (pids, wrapper_start) = parse_parked(&body);
        let Some(wrapper_start) = wrapper_start else {
            let _ = fs::remove_file(entry.path());
            continue;
        };
        if alive_as(wrapper, wrapper_start) {
            continue;
        }
        match parse_cell(&body) {
            Some(path) => {
                if let Some(cell) = Cell::at(path)
                    && !cell.close()
                {
                    continue;
                }
            }
            None => signal(&pids, false),
        }
        let _ = fs::remove_file(entry.path());
    }
}

pub(crate) fn rescue(dir: &Path, tidy: bool) {
    if tidy {
        drop_turn_files(dir);
    }
    drop_dead_holder(dir);
    if tidy {
        drop_stale_served(dir);
        drop_dead_marks(dir, "desk");
        drop_dead_marks(dir, "play");
        drop_dead_marks(dir, "lent");
        drop_dead_marks(dir, "reclaim");
        drop_dead_marks(dir, "plays");
        drop_old_temporaries(dir);
        drop_old_temporaries(&dir.join("tickets"));
    }
    wake_orphans(dir);
}

#[cfg(test)]
mod tests {
    use super::{
        Cell, SERVED_KEEP, TEMPORARY_KEEP, cell_body, cgroup_line, drop_dead_holder,
        drop_turn_files_of, kin, parked_body, parse_cell, parse_parked, rescue, same_second,
        sigstop_body, turn_owner,
    };
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-{name}"));
        let _ = fs::remove_dir_all(&path);
        for sub in ["tickets", "turns", "served", "stopped"] {
            fs::create_dir_all(path.join(sub)).unwrap();
        }
        path
    }

    #[test]
    fn a_turn_tidies_once_a_second() {
        assert!(!same_second(None, 5_000));
        assert!(same_second(Some(5_000), 5_000));
        assert!(same_second(Some(5_000), 5_999));
        assert!(!same_second(Some(5_000), 6_000));
        assert!(!same_second(Some(4_999), 5_000));
    }

    #[test]
    fn rescue_always_drops_a_dead_holder_and_tidies_only_when_asked() {
        let dir = test_dir("rescue-tidy");
        let gone = 4_194_305u32;
        let turn = dir.join("turns").join(format!("{gone}.last"));
        let served = dir.join("served").join("old");
        let fresh = dir.join("served").join("new");
        let running = dir.join("running");
        let set = || {
            fs::write(&turn, "1").unwrap();
            fs::write(&served, "1").unwrap();
            fs::write(&fresh, "1").unwrap();
            let week = SystemTime::now() - SERVED_KEEP - Duration::from_secs(60);
            File::options()
                .write(true)
                .open(&served)
                .unwrap()
                .set_modified(week)
                .unwrap();
            fs::write(&running, format!("a\nx\n{gone}\n\n")).unwrap();
        };
        set();
        rescue(&dir, false);
        assert!(!running.exists());
        assert!(turn.exists() && served.exists() && fresh.exists());
        set();
        rescue(&dir, true);
        assert!(!running.exists() && !turn.exists() && !served.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn rescue_sweeps_the_lent_marks_of_wrappers_that_have_gone() {
        let dir = test_dir("rescue-lent");
        fs::create_dir_all(dir.join("lent")).unwrap();
        let me = std::process::id();
        let start = crate::paths::start_time(me).unwrap();
        let live = dir.join("lent").join(me.to_string());
        let gone = dir.join("lent").join("4194307");
        fs::write(&live, format!("{start}\n")).unwrap();
        fs::write(&gone, format!("{start}\n")).unwrap();
        rescue(&dir, false);
        assert!(live.exists() && gone.exists());
        rescue(&dir, true);
        assert!(live.exists() && !gone.exists());
        fs::write(&live, format!("{}\n", start + 1)).unwrap();
        rescue(&dir, true);
        assert!(!live.exists());
    }

    #[test]
    fn the_turn_files_are_listed_before_the_tickets_are_read() {
        let dir = test_dir("sweep-order");
        let old = dir.join("turns").join("4194308");
        let kept = dir.join("turns").join("4194309.last");
        let late = dir.join("turns").join("4194310");
        let late_lock = dir.join("turns").join("4194310.lock");
        fs::write(&old, "wait").unwrap();
        fs::write(&kept, "1").unwrap();
        drop_turn_files_of(&dir, || {
            fs::write(&late, "wait").unwrap();
            fs::write(&late_lock, "").unwrap();
            vec![4_194_309]
        });
        assert!(!old.exists());
        assert!(kept.exists());
        assert!(
            late.exists() && late_lock.exists(),
            "a wrapper that started between the two reads keeps its turn files"
        );
    }

    fn aged(path: &PathBuf, age: Duration) {
        fs::write(path, "x").unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn the_tidy_pass_sweeps_temporaries_older_than_a_minute() {
        let dir = test_dir("sweep-tmp");
        let me = std::process::id();
        let old = TEMPORARY_KEEP + Duration::from_secs(5);
        let young = Duration::from_secs(1);
        let olds = [
            dir.join(format!("running.{me}.0.tmp")),
            dir.join("tickets").join(format!("1-2-3.{me}.1.tmp")),
            dir.join("turns").join(format!("{me}.{me}.2.tmp")),
        ];
        let youngs = [
            dir.join(format!("running.{me}.3.tmp")),
            dir.join("tickets").join(format!("1-2-3.{me}.4.tmp")),
            dir.join("turns").join(format!("{me}.last.{me}.5.tmp")),
        ];
        let other = dir.join("paused");
        for path in &olds {
            aged(path, old);
        }
        for path in &youngs {
            aged(path, young);
        }
        aged(&other, old);
        rescue(&dir, false);
        assert!(olds.iter().chain(&youngs).all(|path| path.exists()));
        rescue(&dir, true);
        for path in &olds {
            assert!(!path.exists(), "{}", path.display());
        }
        for path in &youngs {
            assert!(path.exists(), "{}", path.display());
        }
        assert!(other.exists());
    }

    #[test]
    fn a_dead_holder_is_dropped_only_while_no_one_holds_the_lock() {
        let dir = test_dir("dead-holder-lock");
        let running = dir.join("running");
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
            .unwrap();
        fs::write(&running, "a\nx\n4194311\n\n").unwrap();
        lock.lock().unwrap();
        drop_dead_holder(&dir);
        assert!(running.exists(), "a new holder may be about to write it");
        lock.unlock().unwrap();
        drop_dead_holder(&dir);
        assert!(!running.exists());
        let me = std::process::id();
        let start = crate::paths::start_time(me).unwrap();
        fs::write(&running, format!("a\nx\n{me}\n{start}\n")).unwrap();
        drop_dead_holder(&dir);
        assert!(running.exists(), "a live holder stays");
        fs::write(&running, format!("a\nx\n{me}\n\n")).unwrap();
        drop_dead_holder(&dir);
        assert!(!running.exists(), "a start-less running file is stale");
    }

    #[test]
    fn a_parked_list_keeps_start_times_and_skips_a_bare_pid() {
        let body = parked_body(&[(10, 500), (11, 600)], 42);
        assert_eq!(body, "10:500 11:600\n42\n");
        assert_eq!(parse_parked(&body), (vec![(10, 500), (11, 600)], Some(42)));
        assert_eq!(parse_parked("7 8 x 9:y 12:13"), (vec![(12, 13)], None));
        assert_eq!(parse_parked("3\n"), (vec![], None));
    }

    #[test]
    fn a_cell_entry_names_its_cgroup_and_an_older_reader_finds_no_pids_in_it() {
        let path = PathBuf::from("/sys/fs/cgroup/user.slice/a b.scope/pgpu-42");
        let body = cell_body(&Cell::at(path.clone()).unwrap(), 9);
        assert_eq!(
            body,
            "cgroup /sys/fs/cgroup/user.slice/a b.scope/pgpu-42\n9\n"
        );
        assert_eq!(parse_cell(&body), Some(path.clone()));
        assert_eq!(parse_parked(&body), (vec![], Some(9)));
        assert_eq!(parse_cell("cgroup \n9\n"), None);
        assert_eq!(parse_cell("10:500\n42\n"), None);
        assert_eq!(
            parse_cell("cgroup /x/pgpu-1\n9\nhome /x\n"),
            Some(PathBuf::from("/x/pgpu-1"))
        );
    }

    #[test]
    fn a_sigstop_entry_is_marked_and_reads_as_a_pid_list() {
        let body = sigstop_body(&[(10, 500), (11, 600)], 42);
        assert_eq!(body, "10:500 11:600\n42\nsigstop\n");
        assert_eq!(parse_parked(&body), (vec![(10, 500), (11, 600)], Some(42)));
        assert_eq!(parse_parked("11\n42\nsigstop\n"), (vec![], Some(42)));
        assert_eq!(parse_cell(&body), None);
    }

    #[test]
    fn the_cgroup_comes_from_the_v2_line() {
        assert_eq!(
            cgroup_line("0::/user.slice/x.scope\n"),
            Some(PathBuf::from("/sys/fs/cgroup/user.slice/x.scope"))
        );
        assert_eq!(
            cgroup_line("1:name=systemd:/old\n0::/a\n"),
            Some(PathBuf::from("/sys/fs/cgroup/a"))
        );
        assert_eq!(cgroup_line("0::/\n"), Some(PathBuf::from("/sys/fs/cgroup")));
        assert_eq!(cgroup_line("1:cpu:/a\n"), None);
        assert_eq!(cgroup_line(""), None);
    }

    #[test]
    fn only_a_pgpu_cgroup_under_the_cgroup_root_is_ever_touched() {
        for good in ["/sys/fs/cgroup/a/pgpu-1", "/sys/fs/cgroup/pgpu-x"] {
            assert!(Cell::at(PathBuf::from(good)).is_some(), "{good}");
        }
        for bad in [
            "/sys/fs/cgroup/a/b.scope",
            "/sys/fs/cgroup",
            "/sys/fs/cgroup/a/pgpu-1/..",
            "/sys/fs/cgroup/a/../pgpu-1",
            "/sys/fs/cgroupx/pgpu-1",
            "sys/fs/cgroup/pgpu-1",
            "/run/user/1000/pgpu-1",
        ] {
            assert!(Cell::at(PathBuf::from(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn rescue_drops_the_cell_entry_of_a_dead_wrapper_without_touching_a_foreign_cgroup() {
        let dir = test_dir("rescue-cell");
        let gone = 4_194_305u32;
        let foreign = dir.join("stopped").join(gone.to_string());
        fs::write(&foreign, "cgroup /sys/fs/cgroup/user.slice\n\n").unwrap();
        let missing = dir.join("stopped").join((gone + 1).to_string());
        fs::write(
            &missing,
            "cgroup /sys/fs/cgroup/pgpu-test-missing/pgpu-4194306\n\n",
        )
        .unwrap();
        rescue(&dir, false);
        assert!(!foreign.exists() && !missing.exists());
    }

    #[test]
    fn a_cell_lists_its_processes() {
        let path = test_dir("cell-ids");
        let cell = Cell {
            path: path.clone(),
            ceiling: None,
        };
        assert!(cell.pids().is_empty());
        fs::write(path.join("cgroup.procs"), "12\n345\nx\n").unwrap();
        assert_eq!(cell.pids(), vec![12, 345]);
        fs::write(path.join("cgroup.procs"), "").unwrap();
        assert!(cell.pids().is_empty());
    }

    #[test]
    fn a_job_counts_every_descendant_even_one_that_left_its_cell() {
        let root = test_dir("kin");
        for (pid, tid, children) in [
            (10, 10, "11 12"),
            (10, 13, "14"),
            (11, 11, "15"),
            (12, 12, ""),
            (14, 14, "10"),
            (20, 20, "21"),
        ] {
            let task = root
                .join(pid.to_string())
                .join("task")
                .join(tid.to_string());
            fs::create_dir_all(&task).unwrap();
            fs::write(task.join("children"), children).unwrap();
        }
        let mut found = kin(&root, &[10, 15]);
        found.sort_unstable();
        assert_eq!(found, vec![10, 11, 12, 14, 15]);
        assert_eq!(kin(&root, &[99]), vec![99]);
    }

    #[test]
    fn turn_files_name_their_wrapper() {
        assert_eq!(turn_owner("123"), Some(123));
        assert_eq!(turn_owner("123.last"), Some(123));
        assert_eq!(turn_owner("123.served"), Some(123));
        assert_eq!(turn_owner("123.pace"), Some(123));
        assert_eq!(turn_owner("123.lock"), Some(123));
        assert_eq!(turn_owner("123.rest"), Some(123));
        assert_eq!(turn_owner("123.frozen"), Some(123));
        assert_eq!(turn_owner("123.wake"), Some(123));
        assert_eq!(turn_owner("123.back"), Some(123));
        assert_eq!(turn_owner("x.last"), None);
        assert_eq!(turn_owner("123.last.9.0.tmp"), None);
    }
}
