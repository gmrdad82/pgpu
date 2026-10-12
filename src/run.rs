use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::budget::{self, Clock, Stop, cpu_busy};
use crate::fake::fake_percent;
use crate::hour;
use crate::knobs::{lend_on, priority_low};
use crate::pace;
use crate::paths::{
    home, millis, now, proc_stat, rewrite_keeping_time, say, start_time, stat_state, state,
    turn_file, write_atomic,
};
use crate::procs::{
    Cell, Proc, catch_endings, cell_body, cpu_of, ended, kin, send, signal, sigstop_body, tree,
    with_starts,
};
use crate::queue::{
    Body, CLASSES, CLIP, Class, Hold, Mark, View, acquire, age, class_of, clean, record,
    running_body, says, session, ticket_body, wake_next,
};
use crate::seats::{Seats, holding};
use crate::spawn::{Kid, spawn};
use crate::usage as gpu_usage;
use crate::wake::{Bell, Exit, LOOK, RING, Rung, back_path, ring, ring_wrapper, wake_path};
use crate::wrapper::{Act, IDLE, Leave, Look, Pace, Seen, Wrapper, signal_code};

pub(crate) const TURN_TICK: Duration = Duration::from_millis(20);

const GRACE: Duration = Duration::from_secs(2);

fn nap(bell: Option<&Bell>, also: Option<RawFd>, ms: Duration) -> Rung {
    match bell {
        Some(bell) => bell.wait(also, ms),
        None => {
            sleep(ms);
            Rung::default()
        }
    }
}

struct Watch {
    job_meter: gpu_usage::Meter,
    job_rate: Option<f64>,
    video: bool,
    kfd: bool,
}

impl Watch {
    fn new() -> Self {
        Self {
            job_meter: gpu_usage::Meter::new(),
            job_rate: None,
            video: false,
            kfd: false,
        }
    }

    fn sample(&mut self, pids: &[u32], who: &str) {
        self.kfd = fake_percent("kfd").map_or_else(|| gpu_usage::uses_kfd(pids), |v| v > 0);
        let job_rate = self.job_meter.push(gpu_usage::sample(pids), Instant::now());
        self.job_rate = fake_percent(&format!("gpu.{who}"))
            .map(|percent| f64::from(percent) / 100.0)
            .or(job_rate.as_ref().map(|rate| rate.combined));
        self.video = job_rate.as_ref().is_some_and(|rate| rate.video());
    }
}

fn owner() -> String {
    let dir = std::env::current_dir().ok();
    let repo = dir.as_deref().and_then(|d| {
        d.ancestors()
            .find(|a| a.join(".git").exists())
            .and_then(|a| a.file_name())
            .map(|n| n.to_string_lossy().into_owned())
    });
    repo.unwrap_or_else(|| "unknown".into())
}

fn exited(child: &mut Kid) -> Option<Leave> {
    match child.try_wait() {
        Ok(Some(status)) => Some(Leave::Done(Some(status))),
        Ok(None) => None,
        Err(_) => Some(Leave::Done(None)),
    }
}

fn signal_name(sig: i32) -> String {
    let name = match sig {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSTKFLT => "SIGSTKFLT",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        libc::SIGURG => "SIGURG",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGVTALRM => "SIGVTALRM",
        libc::SIGPROF => "SIGPROF",
        libc::SIGWINCH => "SIGWINCH",
        libc::SIGIO => "SIGIO",
        libc::SIGPWR => "SIGPWR",
        libc::SIGSYS => "SIGSYS",
        34..=64 => return format!("SIGRTMIN+{}", sig - 34),
        _ => return format!("SIG{sig}"),
    };
    name.to_string()
}

fn death(leave: Leave, cancelled: bool) -> Option<String> {
    let Leave::Done(Some(status)) = leave else {
        return None;
    };
    let sig = status.signal().filter(|_| !cancelled)?;
    Some(format!(
        "pgpu: job died by {} (signal {sig})",
        signal_name(sig)
    ))
}

fn nested(queue: Option<&OsStr>) -> bool {
    queue.is_some_and(|value| !value.is_empty())
}

struct Files {
    ticket: PathBuf,
    turn: PathBuf,
    mark: PathBuf,
    pace: PathBuf,
    frozen: PathBuf,
    turn_lock: PathBuf,
    served: PathBuf,
    parked: PathBuf,
    lent: PathBuf,
    reclaim: PathBuf,
    offered: PathBuf,
    ran: PathBuf,
    wake: PathBuf,
    back: PathBuf,
}

impl Files {
    fn new(dir: &Path, class: u8, me: u32) -> Self {
        Self {
            ticket: dir.join("tickets").join(format!("{class}-{}-{me}", now())),
            turn: turn_file(dir, me, ""),
            mark: turn_file(dir, me, ".last"),
            pace: turn_file(dir, me, ".pace"),
            frozen: pace::frozen_path(dir, me),
            turn_lock: turn_file(dir, me, ".lock"),
            served: turn_file(dir, me, ".served"),
            parked: dir.join("stopped").join(me.to_string()),
            lent: dir.join("lent").join(me.to_string()),
            reclaim: dir.join("reclaim").join(me.to_string()),
            offered: dir.join("offered").join(me.to_string()),
            ran: turn_file(dir, me, ".ran"),
            wake: wake_path(dir, me),
            back: back_path(dir, me),
        }
    }

    fn clear(&self) {
        for path in [
            &self.parked,
            &self.lent,
            &self.reclaim,
            &self.offered,
            &self.mark,
            &self.pace,
            &self.frozen,
            &self.turn_lock,
            &self.served,
            &self.wake,
            &self.back,
            &self.ran,
            &self.ticket,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

struct Card {
    path: PathBuf,
    who: String,
    label: String,
    start: u64,
    started: bool,
    mark: Mark,
    class: &'static Class,
    alone: bool,
    ages_after: Option<Duration>,
    waited: Duration,
}

impl Card {
    fn new(path: &Path, who: &str, label: &str, start: u64, class: &'static Class) -> Self {
        Self {
            path: path.to_path_buf(),
            who: who.to_string(),
            label: label.to_string(),
            start,
            started: false,
            mark: Mark::Plain,
            class,
            alone: class.alone,
            ages_after: (class.rank > CLIP).then(age),
            waited: Duration::ZERO,
        }
    }

    fn body(&self) -> String {
        ticket_body(&Body {
            owner: &self.who,
            label: &self.label,
            started: self.started,
            mark: self.mark,
            start: self.start,
            name: self.class.name,
            alone: self.alone,
        })
    }

    fn write(&self) -> std::io::Result<()> {
        write_atomic(&self.path, &self.body())
    }

    fn marks(&mut self, mark: Mark) {
        if self.mark != Mark::Plain {
            return;
        }
        self.mark = mark;
        let written = if self.started {
            rewrite_keeping_time(&self.path, &self.body())
        } else {
            self.write()
        };
        if written.is_err() {
            self.mark = Mark::Plain;
        }
    }

    fn waiting(&mut self, since: Instant) {
        let due = self
            .ages_after
            .is_some_and(|after| self.waited + since.elapsed() >= after);
        if due {
            self.marks(Mark::Aged);
        }
    }

    fn ran(&mut self, ran: Duration, turned: bool) {
        if self.class.rank != 0 {
            return;
        }
        if ran >= budget::LONG {
            self.marks(Mark::Long);
        } else if ran >= budget::UNPACED && !turned {
            self.marks(Mark::Unpaced);
        }
    }

    fn waited(&mut self, since: Instant) {
        self.waited += since.elapsed();
    }
}

fn fall_back(why: &str) {
    say(&format!(
        "pgpu: {why}, so this job is paused with SIGSTOP instead of the cgroup freezer"
    ));
}

fn enter(files: &Files, me: u32, start: u64, memory: u64) -> Option<(Cell, File)> {
    let cell = match Cell::make(me, memory) {
        Ok(cell) => cell,
        Err(e) => {
            fall_back(&format!("cannot make a cgroup for this job ({e})"));
            say("pgpu: this job has no memory ceiling");
            return None;
        }
    };
    if cell.ceiling.is_none() {
        say("pgpu: this job has no memory ceiling: its cgroup has no memory controller");
    }
    let dir = match cell.dir() {
        Ok(dir) => dir,
        Err(e) => {
            fall_back(&format!("cannot open its cgroup ({e})"));
            cell.close();
            return None;
        }
    };
    let _ = write_atomic(&files.parked, &cell_body(&cell, start));
    Some((cell, dir))
}

fn shut(cell: Option<(Cell, File)>) {
    if let Some((cell, _)) = cell {
        cell.close();
    }
}

fn not_found(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENOENT | libc::EACCES | libc::ENOEXEC | libc::ENOTDIR | libc::ELOOP)
    )
}

fn start_job(
    command: &[String],
    set: &[(&str, &OsString)],
    cell: Option<(Cell, File)>,
    files: &Files,
) -> io::Result<(Kid, Option<Cell>)> {
    let Some((cell, dir)) = cell else {
        return spawn(command, set, None).map(|kid| (kid, None));
    };
    let first = spawn(command, set, Some(dir.as_raw_fd()));
    drop(dir);
    let outside = |kid: Kid, why: &str| {
        fall_back(why);
        cell.close();
        let _ = fs::remove_file(&files.parked);
        Ok((kid, None))
    };
    match first {
        Ok(kid) if cell.holds(kid.id()) => Ok((kid, Some(cell))),
        Ok(kid) => outside(kid, "its first process stayed outside its cgroup"),
        Err(e) if not_found(&e) => {
            cell.close();
            Err(e)
        }
        Err(e) => match spawn(command, set, None) {
            Ok(kid) => outside(kid, &format!("cannot start it in its cgroup ({e})")),
            Err(again) => {
                cell.close();
                Err(again)
            }
        },
    }
}

fn abandon(dir: &Path, files: &Files, seats: &mut Seats, code: u8) -> ExitCode {
    let _ = fs::remove_file(seats.running(dir));
    files.clear();
    seats.release();
    ExitCode::from(code)
}

struct Job {
    dir: PathBuf,
    files: Files,
    card: Card,
    seats: Seats,
    who: String,
    class: u8,
    me: u32,
    start: u64,
    holding_body: String,
    held: Hold,
    holding: bool,
    child: Kid,
    root: u32,
    cell: Option<Cell>,
    parked: Vec<Proc>,
    watch: Watch,
    fold: pace::Fold,
    frozen: pace::Frozen,
    began: u128,
    regained: bool,
    bell: Option<Bell>,
    exit: Option<Exit>,
    clock: Clock,
    cpu: Option<(Instant, Duration)>,
    still: bool,
    sole: Sole,
    share: Share,
    reason: Option<String>,
}

#[derive(Default)]
struct Share {
    others: Vec<u32>,
    busy: u64,
    looks: u64,
}

impl Share {
    fn see(&mut self, me: u32, holding: &[u32], busy: Option<u32>) {
        for pid in holding {
            if *pid != me && !self.others.contains(pid) {
                self.others.push(*pid);
            }
        }
        if let Some(busy) = busy {
            self.busy += u64::from(busy);
            self.looks += 1;
        }
    }

    fn average(&self) -> Option<u64> {
        (self.looks > 0).then(|| self.busy / self.looks)
    }

    fn line(&self, who: &str, class: &str, ran: Duration, reason: Option<&str>) -> String {
        let beside = match self.others.len() {
            0 => "with the card to itself".to_string(),
            1 => "beside 1 other job".to_string(),
            n => format!("beside {n} other jobs"),
        };
        let busy = self.average().map_or(String::new(), |busy| {
            format!(", the GPU {busy}% busy on average")
        });
        let after = match reason {
            Some(why) => format!("; re-run alone: {why}"),
            None => String::new(),
        };
        format!(
            "pgpu: {who} ({class}) ran {} {beside}{busy}{after}",
            budget::span(ran)
        )
    }
}

#[derive(Default)]
struct Sole {
    looked: Option<Instant>,
    idle_since: Option<Instant>,
    held: Duration,
    busy: Duration,
    blind: bool,
    loans: u32,
    lent_at: Option<Instant>,
    lent: Duration,
}

impl Sole {
    fn line(&self, who: &str, class: &str) -> String {
        let busy = if self.blind {
            "its GPU use can't be seen (KFD)".to_string()
        } else {
            let pct = if self.held.is_zero() {
                0.0
            } else {
                100.0 * self.busy.as_secs_f64() / self.held.as_secs_f64()
            };
            format!(
                "the GPU was busy {pct:.0}% of it ({})",
                budget::span(self.busy)
            )
        };
        let lent = match self.loans {
            0 => String::new(),
            1 => format!("; it lent its lanes once, for {}", budget::span(self.lent)),
            n => format!(
                "; it lent its lanes {n} times, for {}",
                budget::span(self.lent)
            ),
        };
        format!(
            "pgpu: {who} ({class}) held the GPU alone {}; {busy}{lent}",
            budget::span(self.held)
        )
    }
}

const GPU_WORK: f64 = 0.01;

const FROZEN_SAID: u128 = 1000;

fn nanos_since(began: u128) -> Duration {
    Duration::from_nanos(u64::try_from(now().saturating_sub(began)).unwrap_or(u64::MAX))
}

const TURNED: Duration = Duration::from_secs(2);

impl Job {
    fn see(&mut self, wrapper: &Wrapper, look_now: bool) -> Seen {
        if let Some(leave) = exited(&mut self.child).or_else(|| ended().map(Leave::Signal)) {
            return Seen::leaving(Instant::now(), leave);
        }
        let last = fs::metadata(&self.files.mark)
            .and_then(|m| m.modified())
            .ok();
        let look = (look_now || wrapper.looks_due(Instant::now())).then(|| self.look());
        Seen {
            now: Instant::now(),
            leave: None,
            look,
            turned_ago: last.and_then(|t| t.elapsed().ok()),
            turn_wait: says(&self.files.turn, "wait"),
            regained: std::mem::take(&mut self.regained),
        }
    }

    fn look(&mut self) -> Look {
        let at = Instant::now();
        let since = self.sole.looked.replace(at);
        self.watch.sample(&self.pids(), &self.who);
        self.fold.fold(&pace::ring(&self.files.pace), self.began);
        let pace = Pace {
            job_gpu: self.watch.job_rate,
            video: self.watch.video,
            kfd: self.watch.kfd,
        };
        self.sole.idle_since = pace.idle(IDLE).then(|| self.sole.idle_since.unwrap_or(at));
        if self.holding && self.card.alone {
            self.sole.blind |= pace.kfd;
            if let (Some(since), Some(rate)) = (since, pace.job_gpu) {
                self.sole.busy += at
                    .saturating_duration_since(since)
                    .mul_f64(rate.clamp(0.0, 1.0));
            }
        }
        let lend = lend_on(self.card.alone);
        let view = self.holding.then(|| View::read(&self.dir));
        if let Some(view) = view.as_ref().filter(|_| self.tells_share()) {
            self.share.see(self.me, &view.holding, crate::gpu::busy());
        }
        Look {
            lend,
            pace,
            next_up: lend && view.as_ref().is_some_and(|view| view.next_up(self.me)),
            yields: !self.card.alone && view.as_ref().is_some_and(|view| view.reclaimed(self.me)),
        }
    }

    fn tells_share(&self) -> bool {
        self.card.alone || self.card.class.name == "bench"
    }

    fn pids(&self) -> Vec<u32> {
        match &self.cell {
            Some(cell) => {
                let (mut inside, outside) = self.spread(cell);
                inside.extend(outside);
                inside
            }
            None => tree(self.root),
        }
    }

    fn spread(&self, cell: &Cell) -> (Vec<u32>, Vec<u32>) {
        let mut seeds = cell.pids();
        let inside = seeds.clone();
        seeds.push(self.root);
        let outside = kin(Path::new("/proc"), &seeds)
            .into_iter()
            .filter(|pid| !inside.contains(pid))
            .collect();
        (inside, outside)
    }

    fn left(&mut self) -> Vec<Proc> {
        let root = matches!(self.child.try_wait(), Ok(None));
        let pids = match &self.cell {
            Some(cell) if root => {
                let (mut inside, outside) = self.spread(cell);
                inside.extend(outside);
                inside
            }
            Some(cell) => kin(Path::new("/proc"), &cell.pids()),
            None if root => tree(self.root),
            None => Vec::new(),
        };
        with_starts(&pids)
    }

    fn tell(&mut self, sig: i32) {
        send(&self.left(), sig);
    }

    fn reap(&mut self, grace: Duration) {
        let until = Instant::now() + grace;
        while Instant::now() < until && !self.left().is_empty() {
            sleep(TURN_TICK);
        }
        let left = self.left();
        if left.is_empty() {
            return;
        }
        if let Some(cell) = &self.cell {
            cell.kill();
        }
        send(&left, libc::SIGKILL);
        let until = Instant::now() + GRACE;
        while Instant::now() < until && !self.left().is_empty() {
            sleep(TURN_TICK);
        }
    }

    fn apply(&mut self, act: Act) -> Option<Leave> {
        if let Some(total) = self.frozen.act(&act, Instant::now()) {
            let _ = write_atomic(&self.files.frozen, &total.to_string());
        }
        match act {
            Act::Park => match &self.cell {
                Some(cell) => {
                    cell.freeze(true);
                }
                None => {
                    let pids = with_starts(&tree(self.root));
                    let _ = write_atomic(&self.files.parked, &sigstop_body(&pids, self.start));
                    signal(&pids, true);
                    self.parked = pids;
                }
            },
            Act::Resume => match &self.cell {
                Some(cell) => {
                    cell.freeze(false);
                }
                None => {
                    signal(&std::mem::take(&mut self.parked), false);
                    let _ = fs::remove_file(&self.files.parked);
                }
            },
            Act::HandOver => return self.give_way().err(),
            Act::Lend => self.lend(),
            Act::TakeBack => return self.take_back().err(),
            Act::Reclaim => return self.reclaim().err(),
            Act::Yield => return self.give_back().err(),
            Act::Exit(leave) => return Some(leave),
        }
        None
    }

    fn cpu_now(&self) -> Option<Duration> {
        match &self.cell {
            Some(cell) => cell.cpu().map(|spent| spent + cpu_of(&self.spread(cell).1)),
            None => Some(cpu_of(&tree(self.root))),
        }
    }

    fn judge(&mut self, seen: &Seen) -> Option<Stop> {
        let look = seen.look.as_ref()?;
        let at = Instant::now();
        let cpu = self.cpu_now();
        let cpu_worked = match (self.cpu, cpu) {
            (Some((then, before)), Some(spent)) => cpu_busy(
                spent.saturating_sub(before),
                at.saturating_duration_since(then),
            ),
            _ => false,
        };
        self.cpu = cpu.map(|spent| (at, spent));
        self.still = proc_stat(Path::new("/proc"), self.root)
            .as_deref()
            .and_then(stat_state)
            .is_some_and(|state| matches!(state, 'T' | 't'));
        let pace = &look.pace;
        if pace.job_gpu.is_some_and(|gpu| gpu >= GPU_WORK)
            || pace.video
            || pace.kfd
            || cpu_worked
            || seen.turned_ago.is_some_and(|ago| ago < TURNED)
        {
            self.clock.busy();
        }
        self.card.ran(self.clock.ran, seen.turned_ago.is_some());
        if self.clock.passed() {
            say(&format!(
                "pgpu: {} ({}) passed its {} budget; it is stopped at twice that",
                self.who,
                self.card.class.name,
                budget::span(self.clock.budget)
            ));
        }
        let _ = write_atomic(
            &self.files.ran,
            &format!(
                "{} {} {} {} {}\n",
                self.clock.ran.as_millis(),
                self.clock.budget.as_millis(),
                self.clock.quiet().as_millis(),
                self.frozen_ms(),
                self.sole
                    .idle_since
                    .map_or(0, |since| since.elapsed().as_millis())
            ),
        );
        self.clock.verdict()
    }

    fn killed(&self, why: &str) {
        hour::append(
            &state().join("killed.tsv"),
            &format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                millis(),
                self.who,
                self.card.class.name,
                self.me,
                self.clock.budget.as_millis(),
                self.clock.ran.as_millis(),
                why,
                self.card.label,
                self.cell
                    .as_ref()
                    .and_then(|cell| cell.ceiling)
                    .map_or(String::new(), |bytes| bytes.to_string())
            ),
        );
    }

    fn out_of_memory(&self, leave: Leave) -> Option<String> {
        let Leave::Done(Some(status)) = leave else {
            return None;
        };
        let cell = self.cell.as_ref()?;
        if status.success() || !cell.out_of_memory() {
            return None;
        }
        let peak = cell.peak().map_or(String::new(), |peak| {
            format!(", peak {}", budget::size(peak))
        });
        Some(match cell.ceiling {
            Some(ceiling) => format!(
                "pgpu: stopped {} ({}): out of memory at its {} ceiling{peak}; a bigger job passes --memory",
                self.who,
                self.card.class.name,
                budget::size(ceiling)
            ),
            None => format!(
                "pgpu: stopped {} ({}): killed for memory{peak}",
                self.who, self.card.class.name
            ),
        })
    }

    fn terminate(&mut self, stop: Stop) {
        let line = match stop {
            Stop::Budget => format!(
                "pgpu: stopped {} ({}): ran {}, twice its {} budget; a longer job passes --budget",
                self.who,
                self.card.class.name,
                budget::span(self.clock.ran),
                budget::span(self.clock.budget)
            ),
            Stop::Stall => format!(
                "pgpu: stopped {} ({}): stalled, no GPU work, turn or CPU for {} (budget {})",
                self.who,
                self.card.class.name,
                budget::span(self.clock.quiet()),
                budget::span(self.clock.budget)
            ),
        };
        say(&line);
        self.killed(stop.word());
        self.tell(libc::SIGTERM);
        self.reap(budget::grace());
    }

    fn step_aside(&mut self, end: &str) {
        self.held_alone();
        let stamp = now().to_string();
        let _ = write_atomic(&self.dir.join("served").join(&self.who), &stamp);
        let _ = write_atomic(&self.files.served, &stamp);
        let _ = fs::remove_file(self.seats.running(&self.dir));
        record(&self.who, self.class, &self.held, self.me, end);
    }

    fn mark(&self, path: &Path) {
        let _ = write_atomic(path, &format!("{}\n", self.start));
    }

    fn go(&self) {
        let _ = write_atomic(&self.files.turn, "go");
        ring(&self.files.back, RING);
    }

    fn release_turn(&self) {
        if says(&self.files.turn, "wait") {
            self.go();
        }
    }

    fn held_alone(&mut self) {
        if self.holding && self.card.alone {
            let ms = millis().saturating_sub(self.held.from);
            self.sole.held += Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX));
        }
    }

    fn regain(&mut self, reclaiming: bool) -> Result<(), Leave> {
        let asked = Instant::now();
        let child = &mut self.child;
        let card = &mut self.card;
        let bell = self.bell.as_ref();
        let exit = self.exit.as_ref().map(Exit::fd);
        let dir = &self.dir;
        let me = self.me;
        acquire(
            dir,
            &mut self.seats,
            me,
            true,
            &mut || {
                card.waiting(asked);
                ended().map(Leave::Signal).or_else(|| exited(child))
            },
            &mut |ms| {
                if reclaiming {
                    for pid in holding(dir).into_iter().filter(|pid| *pid != me) {
                        ring_wrapper(dir, pid, LOOK);
                    }
                }
                nap(bell, exit, ms);
            },
        )?;
        self.card.waited(asked);
        self.holding = true;
        self.held = Hold {
            from: millis(),
            waited: asked.elapsed().as_millis(),
        };
        let _ = write_atomic(&self.seats.running(&self.dir), &self.holding_body);
        Ok(())
    }

    fn give_way(&mut self) -> Result<(), Leave> {
        self.step_aside("turn");
        self.seats.release();
        self.holding = false;
        wake_next(&self.dir, self.me);
        let asked = Instant::now();
        self.regain(false)?;
        let total = self.frozen.waited(asked.elapsed());
        let _ = write_atomic(&self.files.frozen, &total.to_string());
        self.regained = true;
        self.go();
        Ok(())
    }

    fn frozen_ms(&self) -> u128 {
        u128::from(self.frozen.ms(Instant::now()))
    }

    fn lend(&mut self) {
        self.step_aside("lend");
        self.mark(&self.files.lent);
        self.seats.release();
        self.holding = false;
        if self.card.alone {
            self.sole.loans += 1;
            self.sole.lent_at = Some(Instant::now());
        }
        wake_next(&self.dir, self.me);
    }

    fn take_back(&mut self) -> Result<(), Leave> {
        let _ = fs::remove_file(&self.files.lent);
        self.regain(false)?;
        self.regained = true;
        self.release_turn();
        Ok(())
    }

    fn reclaim(&mut self) -> Result<(), Leave> {
        if let Some(at) = self.sole.lent_at.take() {
            self.sole.lent += at.elapsed();
        }
        if let Some(folder) = self.files.reclaim.parent() {
            let _ = fs::create_dir_all(folder);
        }
        self.mark(&self.files.reclaim);
        let _ = fs::remove_file(&self.files.lent);
        let got = self.regain(true);
        let _ = fs::remove_file(&self.files.reclaim);
        got?;
        self.regained = true;
        self.release_turn();
        Ok(())
    }

    fn give_back(&mut self) -> Result<(), Leave> {
        self.step_aside("reclaim");
        self.seats.release();
        self.holding = false;
        for pid in View::read(&self.dir).reclaimers() {
            ring_wrapper(&self.dir, *pid, RING);
        }
        self.regain(false)?;
        self.regained = true;
        self.release_turn();
        Ok(())
    }

    fn end(mut self, leave: Leave) -> ExitCode {
        let memory = self.out_of_memory(leave);
        let died = match &memory {
            Some(line) => {
                self.killed("memory");
                Some(line.clone())
            }
            None => death(leave, ended().is_some()),
        };
        let end = match leave {
            Leave::Signal(sig) => {
                if !self.holding {
                    let _ = fs::remove_file(&self.files.ticket);
                }
                self.tell(sig);
                self.release_turn();
                self.reap(GRACE);
                "signal"
            }
            Leave::Stopped(stop) => {
                self.terminate(stop);
                stop.word()
            }
            Leave::Done(_) => {
                self.reap(Duration::ZERO);
                if memory.is_some() { "memory" } else { "done" }
            }
        };
        if says(&self.files.turn, "wait") {
            self.go();
        } else {
            let _ = fs::remove_file(&self.files.turn);
        }
        self.held_alone();
        if let Some(at) = self.sole.lent_at.take() {
            self.sole.lent += at.elapsed();
        }
        let frozen = self.frozen_ms();
        if frozen >= FROZEN_SAID {
            say(&format!(
                "pgpu: {} ran {} of its {} wall time; {} of it frozen or waiting for the GPU, so wall-clock numbers it took are skewed (GPU_QUEUE_FROZEN has the frozen part)",
                self.who,
                budget::span(self.clock.ran),
                budget::span(nanos_since(self.began)),
                budget::span(std::time::Duration::from_millis(
                    u64::try_from(frozen).unwrap_or(u64::MAX)
                ))
            ));
        }
        if self.card.alone {
            say(&self.sole.line(&self.who, self.card.class.name));
        }
        if self.tells_share() && !self.clock.ran.is_zero() {
            say(&self.share.line(
                &self.who,
                self.card.class.name,
                self.clock.ran,
                self.reason.as_deref(),
            ));
            hour::append(
                &state().join("benches.tsv"),
                &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    millis(),
                    self.who,
                    self.card.class.name,
                    self.me,
                    self.clock.ran.as_millis(),
                    self.share.others.len(),
                    self.share
                        .average()
                        .map_or(String::new(), |busy| busy.to_string()),
                    self.reason.as_deref().unwrap_or(""),
                    self.card.label
                ),
            );
        }
        self.fold.fold(&pace::ring(&self.files.pace), self.began);
        hour::append(
            &state().join("pace.tsv"),
            &self.fold.line(millis(), &self.who, self.class),
        );
        if let Some(cell) = self.cell.take() {
            cell.close();
        }
        self.files.clear();
        if self.holding {
            let _ = write_atomic(&self.dir.join("served").join(&self.who), &now().to_string());
            let _ = fs::remove_file(self.seats.running(&self.dir));
            record(&self.who, self.class, &self.held, self.me, end);
            self.seats.release();
            wake_next(&self.dir, self.me);
        }
        if let Some(line) = died {
            say(&line);
        }
        ExitCode::from(leave.code())
    }
}

fn class_from(given: Option<&str>, env: Option<&str>) -> &'static Class {
    given
        .and_then(class_of)
        .or_else(|| env.and_then(|name| class_of(name.trim())))
        .unwrap_or(&CLASSES[0])
}

pub(crate) struct Ask<'a> {
    pub(crate) class: Option<&'a str>,
    pub(crate) who: Option<String>,
    pub(crate) budget: Option<Duration>,
    pub(crate) memory: Option<u64>,
    pub(crate) alone: Option<String>,
    pub(crate) command: &'a [String],
}

pub(crate) fn run(ask: Ask) -> ExitCode {
    let command = ask.command;
    let who = ask.who.unwrap_or_else(owner);
    if nested(std::env::var_os("GPU_QUEUE").as_deref()) {
        let e = Command::new(&command[0]).args(&command[1..]).exec();
        say(&format!("pgpu: cannot run {}: {e}", command[0]));
        return ExitCode::from(127);
    }
    let kind = class_from(ask.class, std::env::var("PGPU_CLASS").ok().as_deref());
    let class = kind.rank;
    let budget = ask.budget.unwrap_or(kind.budget);
    let memory = ask.memory.unwrap_or(kind.memory);
    let who = session(&who);
    let label = clean(&command.join(" "));
    let dir = home();
    let me = std::process::id();
    let Some(start) = start_time(me) else {
        say("pgpu: cannot read this process's start time");
        return ExitCode::from(1);
    };
    let files = Files::new(&dir, class, me);
    let mut card = Card::new(&files.ticket, &who, &label, start, kind);
    card.alone |= ask.alone.is_some();
    let bell = Bell::make(&files.wake);
    if let Some(bell) = &bell {
        bell.on_signal();
    }
    catch_endings();
    if card.write().is_err() {
        say(&format!("pgpu: cannot write {}", files.ticket.display()));
        files.clear();
        return ExitCode::from(1);
    }
    let mut seats = match Seats::open(&dir) {
        Ok(seats) => seats,
        Err(e) => {
            say(&format!("pgpu: cannot open lock: {e}"));
            files.clear();
            return ExitCode::from(1);
        }
    };
    let cell = enter(&files, me, start, memory);
    if let Some(why) = &ask.alone {
        say(&format!(
            "pgpu: re-runs alone ({why}): it waits for an empty queue or the night ({:02}:00 to {:02}:00), and no other job runs beside it",
            crate::queue::NIGHT.0,
            crate::queue::NIGHT.1
        ));
    } else if card.alone {
        say("pgpu: runs alone: it waits for the GPU to clear and no other job runs beside it");
    }
    let asked = Instant::now();
    if let Err(leave) = acquire(
        &dir,
        &mut seats,
        me,
        false,
        &mut || {
            card.waiting(asked);
            ended().map(Leave::Signal)
        },
        &mut |ms| {
            nap(bell.as_ref(), None, ms);
        },
    ) {
        shut(cell);
        files.clear();
        return ExitCode::from(leave.code());
    }
    card.waited(asked);
    card.started = true;
    let _ = card.write();
    let held = Hold {
        from: millis(),
        waited: asked.elapsed().as_millis(),
    };
    let holding_body = running_body(&who, &label, me, start);
    let _ = write_atomic(&seats.running(&dir), &holding_body);
    let _ = write_atomic(&files.frozen, "0");
    let mut set: Vec<(&str, OsString)> = vec![
        ("GPU_QUEUE", "1".into()),
        ("GPU_QUEUE_TURN", files.turn.clone().into_os_string()),
        ("GPU_QUEUE_PID", me.to_string().into()),
        ("GPU_QUEUE_FROZEN", files.frozen.clone().into_os_string()),
    ];
    if priority_low() && std::env::var_os("AMD_PRIORITY").is_none() {
        set.push(("AMD_PRIORITY", "-512".into()));
    }
    let set: Vec<(&str, &OsString)> = set.iter().map(|(key, value)| (*key, value)).collect();
    if let Some(sig) = ended() {
        shut(cell);
        return abandon(&dir, &files, &mut seats, signal_code(sig));
    }
    let (child, cell) = match start_job(command, &set, cell, &files) {
        Ok(started) => started,
        Err(e) => {
            say(&format!("pgpu: cannot run {}: {e}", command[0]));
            return abandon(&dir, &files, &mut seats, 127);
        }
    };
    let began = now();
    let root = child.id();
    let exit = Exit::of(root);
    let mut wrapper = Wrapper::new(Instant::now(), card.alone);
    let mut job = Job {
        dir,
        files,
        card,
        seats,
        who,
        class,
        me,
        start,
        holding_body,
        held,
        holding: true,
        child,
        root,
        cell,
        parked: Vec::new(),
        watch: Watch::new(),
        fold: pace::Fold::default(),
        frozen: pace::Frozen::default(),
        began,
        regained: false,
        bell,
        exit,
        clock: Clock::new(budget, budget::stall()),
        cpu: None,
        still: false,
        sole: Sole::default(),
        share: Share::default(),
        reason: ask.alone,
    };
    let mut look_now = false;
    let leave = 'running: loop {
        let seen = job.see(&wrapper, look_now);
        if seen.leave.is_none()
            && let Some(stop) = job.judge(&seen)
        {
            break 'running Leave::Stopped(stop);
        }
        let (next, acts) = wrapper.decide(&seen);
        wrapper = next;
        for act in acts {
            if let Some(leave) = job.apply(act) {
                break 'running leave;
            }
        }
        let napped = Instant::now();
        look_now = nap(
            job.bell.as_ref(),
            job.exit.as_ref().map(Exit::fd),
            TURN_TICK,
        )
        .look;
        job.clock
            .tick(napped.elapsed(), wrapper.runs() && !job.still);
    };
    for act in wrapper.finish() {
        job.apply(act);
    }
    job.end(leave)
}

#[cfg(test)]
mod tests {
    use super::{Card, class_from, death, nested, signal_name};
    use crate::queue::{AGE, CLASSES, Class, Mark, rank_name};

    fn of(rank: u8) -> &'static Class {
        CLASSES.iter().find(|c| c.name == rank_name(rank)).unwrap()
    }
    use crate::wrapper::Leave;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;
    use std::time::{Duration, Instant, SystemTime};

    fn ticket_in(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("2-1-7")
    }

    fn ago(minutes: u64) -> Instant {
        Instant::now() - Duration::from_secs(minutes * 60)
    }

    #[test]
    fn a_truth_or_search_ticket_ages_once_its_waits_add_up_to_10_minutes() {
        assert_eq!(AGE, Duration::from_secs(600));
        let path = ticket_in("card-age");
        let mut card = Card::new(&path, "studio", "pfx bake", 9, of(2));
        assert_eq!(card.ages_after, Some(AGE));
        card.write().unwrap();
        card.waiting(ago(6));
        assert!(!(card.mark == Mark::Aged));
        card.waited(ago(6));
        card.waiting(ago(3));
        assert!(!(card.mark == Mark::Aged), "9 minutes in all");
        card.waiting(ago(4));
        assert!((card.mark == Mark::Aged), "6 and 4 minutes");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "studio\npfx bake\naged\n\n9\ntruth\n\n"
        );
        card.started = true;
        card.write().unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "studio\npfx bake\naged\nran\n9\ntruth\n\n",
            "starting keeps the mark"
        );
        assert!(Card::new(&path, "a", "x", 9, of(3)).ages_after.is_some());
        for class in [0, 1] {
            let mut card = Card::new(&path, "a", "x", 9, of(class));
            card.waited(ago(60));
            card.waiting(ago(60));
            assert!(!(card.mark == Mark::Aged), "class {class} never ages");
        }
    }

    #[test]
    fn a_started_ticket_that_ages_keeps_its_start_mtime_and_a_missing_one_ages_later() {
        let path = ticket_in("card-mtime");
        let mut card = Card::new(&path, "paper", "search", 9, of(3));
        card.started = true;
        card.write().unwrap();
        let began = SystemTime::now() - Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(began)
            .unwrap();
        card.waiting(ago(11));
        assert!((card.mark == Mark::Aged));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "paper\nsearch\naged\nran\n9\nsearch\n\n"
        );
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), began);
        let gone = ticket_in("card-gone");
        let mut card = Card::new(&gone, "paper", "search", 9, of(3));
        card.started = true;
        card.waiting(ago(11));
        assert!(!(card.mark == Mark::Aged), "no ticket to mark");
        card.write().unwrap();
        card.waiting(ago(11));
        assert!((card.mark == Mark::Aged));
    }

    #[test]
    fn signals_have_the_kernels_names() {
        assert_eq!(signal_name(libc::SIGSEGV), "SIGSEGV");
        assert_eq!(signal_name(libc::SIGABRT), "SIGABRT");
        assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
        assert_eq!(signal_name(libc::SIGHUP), "SIGHUP");
        assert_eq!(signal_name(libc::SIGSYS), "SIGSYS");
        assert_eq!(signal_name(34), "SIGRTMIN+0");
        assert_eq!(signal_name(64), "SIGRTMIN+30");
        assert_eq!(signal_name(32), "SIG32");
    }

    #[test]
    fn a_death_by_a_signal_says_so_unless_the_wrapper_was_ended() {
        let segv = Leave::Done(Some(ExitStatus::from_raw(11)));
        assert_eq!(
            death(segv, false).as_deref(),
            Some("pgpu: job died by SIGSEGV (signal 11)")
        );
        assert_eq!(death(segv, true), None);
        assert_eq!(death(Leave::Signal(15), false), None);
        assert_eq!(
            death(Leave::Done(Some(ExitStatus::from_raw(3 << 8))), false),
            None
        );
        assert_eq!(
            death(Leave::Done(Some(ExitStatus::from_raw(0))), false),
            None
        );
        assert_eq!(death(Leave::Done(None), false), None);
    }

    #[test]
    fn a_run_is_nested_when_gpu_queue_is_set_and_not_empty() {
        assert!(nested(Some(OsStr::new("1"))));
        assert!(!nested(Some(OsStr::new(""))));
        assert!(!nested(None));
    }

    #[test]
    fn pgpu_class_gives_the_class_when_no_class_is_given_and_an_explicit_one_wins() {
        let name = |given, env| class_from(given, env).name;
        assert_eq!(name(None, None), "interactive");
        assert_eq!(name(None, Some("clip")), "clip");
        assert_eq!(name(None, Some(" bench\n")), "bench");
        assert_eq!(name(None, Some("")), "interactive", "empty");
        assert_eq!(name(None, Some("warp")), "interactive", "unknown");
        assert_eq!(name(None, Some("CLIP")), "interactive", "names are exact");
        assert_eq!(name(Some("interactive"), Some("clip")), "interactive");
        assert_eq!(name(Some("search"), None), "search");
    }

    #[test]
    fn the_classes_keep_their_ranks_budgets_and_a_bench_takes_a_lane_like_the_rest() {
        let class = |name| class_from(Some(name), None);
        let row = |name| {
            let c = class(name);
            (c.rank, c.budget.as_secs() / 60, c.alone)
        };
        assert_eq!(row("interactive"), (0, 5, false));
        assert_eq!(row("test"), (1, 10, false));
        assert_eq!(row("clip"), (1, 25, false));
        assert_eq!(row("render"), (1, 25, false));
        assert_eq!(row("bench"), (1, 20, false));
        assert_eq!(row("truth"), (2, 60, false));
        assert_eq!(row("search"), (3, 60, false));
    }

    #[test]
    fn an_interactive_job_takes_turns_as_clip_after_60_s_or_5_s_without_a_turn() {
        let path = ticket_in("card-slow");
        let mut card = Card::new(&path, "studio", "render", 9, of(0));
        card.started = true;
        card.write().unwrap();
        card.ran(Duration::from_secs(4), false);
        assert_eq!(card.mark, Mark::Plain);
        card.ran(Duration::from_secs(30), true);
        assert_eq!(card.mark, Mark::Plain, "it turns");
        card.ran(Duration::from_secs(60), true);
        assert_eq!(card.mark, Mark::Long);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("studio\nrender\nlong\n")
        );
        let mut quiet = Card::new(&path, "studio", "render", 9, of(0));
        quiet.ran(Duration::from_secs(5), false);
        assert_eq!(quiet.mark, Mark::Unpaced);
        let mut clip = Card::new(&path, "pfx", "clip", 9, of(1));
        clip.ran(Duration::from_secs(600), false);
        assert_eq!(clip.mark, Mark::Plain, "only interactive is demoted");
    }
}
