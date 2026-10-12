use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crate::fake::fake;
use crate::hour;
use crate::paths::{
    home, millis, read_small, say, say_out, stat_start, stat_state, state, write_atomic,
};
use crate::seats::{self, Line, Seats, Wait, holding};
use crate::wake::{RING, ring_wrapper};
use crate::wrapper::Leave;

pub(crate) struct Class {
    pub(crate) name: &'static str,
    pub(crate) rank: u8,
    pub(crate) budget: Duration,
    pub(crate) alone: bool,
    pub(crate) memory: u64,
}

const fn class(name: &'static str, rank: u8, minutes: u64, alone: bool) -> Class {
    Class {
        name,
        rank,
        budget: Duration::from_secs(minutes * 60),
        alone,
        memory: crate::budget::MEMORY,
    }
}

pub(crate) const CLASSES: [Class; 7] = [
    class("interactive", 0, 5, false),
    class("test", 1, 10, false),
    class("clip", 1, 25, false),
    class("render", 1, 25, false),
    class("bench", 1, 20, false),
    class("truth", 2, 60, false),
    class("search", 3, 60, false),
];

pub(crate) const NAMES: [&str; 7] = [
    "interactive",
    "test",
    "clip",
    "render",
    "bench",
    "truth",
    "search",
];

pub(crate) fn class_of(name: &str) -> Option<&'static Class> {
    CLASSES.iter().find(|class| class.name == name)
}

pub(crate) fn rank_name(rank: u8) -> &'static str {
    match rank {
        0 => "interactive",
        1 => "clip",
        2 => "truth",
        3 => "search",
        _ => "?",
    }
}

pub(crate) const CLIP: u8 = 1;

pub(crate) const AGE: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mark {
    #[default]
    Plain,
    Aged,
    Long,
    Unpaced,
}

impl Mark {
    fn word(self) -> &'static str {
        match self {
            Mark::Plain => "",
            Mark::Aged => "aged",
            Mark::Long => "long",
            Mark::Unpaced => "unpaced",
        }
    }

    fn read(word: &str) -> Self {
        match word.trim() {
            "aged" => Mark::Aged,
            "long" => Mark::Long,
            "unpaced" => Mark::Unpaced,
            _ => Mark::Plain,
        }
    }

    pub(crate) fn note(self) -> Option<&'static str> {
        match self {
            Mark::Plain => None,
            Mark::Aged => Some("waited 10 min: takes turns as clip"),
            Mark::Long => Some("interactive for over 60 s: takes turns as clip"),
            Mark::Unpaced => Some("interactive that never calls turn: takes turns as clip"),
        }
    }
}

pub(crate) fn age() -> Duration {
    fake("age_ms")
        .and_then(|ms| ms.parse().ok())
        .map_or(AGE, Duration::from_millis)
}

pub(crate) const NIGHT: (u8, u8) = (1, 7);

pub(crate) fn night() -> bool {
    match fake("night") {
        Some(set) => set == "1",
        None => (NIGHT.0..NIGHT.1).contains(&crate::report::local_hour()),
    }
}

pub(crate) const PASS: Duration = Duration::from_secs(3);

pub(crate) fn pass() -> Duration {
    fake("pass_ms")
        .and_then(|ms| ms.parse().ok())
        .map_or(PASS, Duration::from_millis)
}

fn offers(dir: &Path, list: &[Ticket]) -> Vec<(u32, u128)> {
    list.iter()
        .filter_map(|t| {
            let body = fs::read_to_string(dir.join("offered").join(t.pid.to_string())).ok()?;
            let mut lines = body.lines();
            let start: u64 = lines.next()?.trim().parse().ok()?;
            let since: u128 = lines.next()?.trim().parse().ok()?;
            (start == t.start).then_some((t.pid, since))
        })
        .collect()
}

pub(crate) fn rerun(t: &Ticket, holding: &[u32]) -> bool {
    t.alone && !t.started && !holding.contains(&t.pid)
}

const TICK: Duration = Duration::from_millis(200);

const NAME_MAX: usize = 255;

#[derive(Clone)]
pub(crate) struct Ticket {
    pub(crate) class: u8,
    pub(crate) arrived: u128,
    pub(crate) pid: u32,
    pub(crate) owner: String,
    pub(crate) label: String,
    pub(crate) started: bool,
    pub(crate) mark: Mark,
    pub(crate) start: u64,
    pub(crate) state: Option<char>,
    pub(crate) name: String,
    pub(crate) alone: bool,
}

impl Ticket {
    #[cfg(test)]
    pub(crate) fn bare(class: u8, pid: u32, owner: &str) -> Self {
        Self {
            class,
            arrived: u128::from(pid),
            pid,
            owner: owner.to_string(),
            label: String::new(),
            started: false,
            mark: Mark::Plain,
            start: 0,
            state: None,
            name: rank_name(class).to_string(),
            alone: false,
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        matches!(self.state, Some('T' | 't'))
    }

    pub(crate) fn ranks_as(&self) -> u8 {
        match self.mark {
            Mark::Aged => self.class.min(CLIP),
            Mark::Long | Mark::Unpaced => self.class.max(CLIP),
            Mark::Plain => self.class,
        }
    }

    pub(crate) fn note(&self) -> Option<&'static str> {
        (self.ranks_as() != self.class)
            .then(|| self.mark.note())
            .flatten()
    }
}

pub(crate) struct Body<'a> {
    pub(crate) owner: &'a str,
    pub(crate) label: &'a str,
    pub(crate) started: bool,
    pub(crate) mark: Mark,
    pub(crate) start: u64,
    pub(crate) name: &'a str,
    pub(crate) alone: bool,
}

pub(crate) fn ticket_body(body: &Body) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        body.owner,
        body.label,
        body.mark.word(),
        if body.started { "ran" } else { "" },
        body.start,
        body.name,
        if body.alone { "alone" } else { "" },
    )
}

#[derive(Debug, PartialEq)]
struct Parsed {
    owner: String,
    label: String,
    started: bool,
    mark: Mark,
    start: u64,
    name: Option<String>,
    alone: bool,
}

fn parse_ticket(body: &str) -> Option<Parsed> {
    let mut lines = body.lines();
    let owner = lines.next().unwrap_or("").to_string();
    let label = lines.next().unwrap_or("").to_string();
    let mark = Mark::read(lines.next().unwrap_or(""));
    let started = lines.next().is_some_and(|v| v.trim() == "ran");
    let start = lines.next()?.trim().parse().ok()?;
    let name = lines
        .next()
        .map(str::trim)
        .filter(|name| class_of(name).is_some())
        .map(str::to_string);
    let alone = lines.next().is_some_and(|v| v.trim() == "alone");
    Some(Parsed {
        owner,
        label,
        started,
        mark,
        start,
        name,
        alone,
    })
}

enum Answer {
    Gone,
    Here(Option<char>),
}

fn answer(proc_root: &Path, pid: u32, start: u64) -> Answer {
    let folder = proc_root.join(pid.to_string());
    match read_small(&folder.join("stat")) {
        Ok(stat) => {
            if stat_start(&stat).is_some_and(|read| read != start) {
                Answer::Gone
            } else {
                Answer::Here(stat_state(&stat))
            }
        }
        Err(_) if fs::symlink_metadata(&folder).is_err_and(|e| e.kind() == ErrorKind::NotFound) => {
            Answer::Gone
        }
        Err(_) => Answer::Here(None),
    }
}

pub(crate) fn tickets(dir: &Path) -> Vec<Ticket> {
    tickets_in(dir, Path::new("/proc"))
}

fn tickets_in(dir: &Path, proc_root: &Path) -> Vec<Ticket> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir.join("tickets")) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let parts: Vec<&str> = name.splitn(3, '-').collect();
        let (Some(class), Some(arrived), Some(pid)) = (
            parts.first().and_then(|v| v.parse().ok()),
            parts.get(1).and_then(|v| v.parse().ok()),
            parts.get(2).and_then(|v| v.parse().ok()),
        ) else {
            continue;
        };
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(parsed) = parse_ticket(&body) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        let Answer::Here(state) = answer(proc_root, pid, parsed.start) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        out.push(Ticket {
            class,
            arrived,
            pid,
            owner: parsed.owner,
            label: parsed.label,
            started: parsed.started,
            mark: parsed.mark,
            start: parsed.start,
            state,
            name: parsed.name.unwrap_or_else(|| rank_name(class).to_string()),
            alone: false,
        });
    }
    out
}

fn stamp_in(path: &Path) -> u128 {
    fs::read_to_string(path)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

fn served(dir: &Path, owner: &str) -> u128 {
    stamp_in(&dir.join("served").join(owner))
}

fn ticket_served(dir: &Path, pid: u32) -> u128 {
    stamp_in(&dir.join("turns").join(format!("{pid}.served")))
}

pub(crate) fn rank(
    dir: &Path,
    mut list: Vec<Ticket>,
    giving: Option<(&str, u32, u128)>,
) -> Vec<Ticket> {
    list.sort_by_cached_key(|t| {
        let (owner_served, own_served) = match giving {
            Some((owner, pid, stamp)) => (
                if t.owner == owner {
                    stamp
                } else {
                    served(dir, &t.owner)
                },
                if t.pid == pid {
                    stamp
                } else {
                    ticket_served(dir, t.pid)
                },
            ),
            None => (served(dir, &t.owner), ticket_served(dir, t.pid)),
        };
        (t.ranks_as(), owner_served, own_served, t.arrived, t.pid)
    });
    list
}

pub(crate) fn order(dir: &Path, list: Vec<Ticket>) -> Vec<Ticket> {
    rank(dir, list, None)
}

pub(crate) fn running_body(owner: &str, label: &str, pid: u32, start: u64) -> String {
    format!("{owner}\n{label}\n{pid}\n{start}\n")
}

pub(crate) fn holder_of(body: &str) -> Option<(u32, u64)> {
    let mut lines = body.lines().skip(2);
    let pid = lines.next()?.trim().parse().ok()?;
    let start = lines.next()?.trim().parse().ok()?;
    Some((pid, start))
}

pub(crate) fn clean(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

pub(crate) fn session(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    out.truncate(NAME_MAX);
    if matches!(out.as_str(), "" | "." | "..") {
        return "unknown".to_string();
    }
    out
}

pub(crate) fn marked(dir: &Path, folder: &str, list: &[Ticket]) -> Vec<u32> {
    list.iter()
        .filter(|ticket| {
            fs::read_to_string(dir.join(folder).join(ticket.pid.to_string()))
                .is_ok_and(|body| mark_start(&body) == Some(ticket.start))
        })
        .map(|ticket| ticket.pid)
        .collect()
}

pub(crate) fn lent_marked(dir: &Path, list: &[Ticket]) -> Vec<u32> {
    marked(dir, "lent", list)
}

pub(crate) fn reclaim_marked(dir: &Path, list: &[Ticket]) -> Vec<u32> {
    marked(dir, "reclaim", list)
}

pub(crate) fn mark_start(body: &str) -> Option<u64> {
    body.lines().next().and_then(|v| v.trim().parse().ok())
}

pub(crate) struct Hold {
    pub(crate) from: u128,
    pub(crate) waited: u128,
}

pub(crate) struct Gate {
    lent: Vec<u32>,
    stopped: Vec<u32>,
    reclaim: Vec<u32>,
}

impl Gate {
    pub(crate) fn read(dir: &Path, list: &[Ticket]) -> Self {
        Self {
            lent: lent_marked(dir, list),
            reclaim: reclaim_marked(dir, list),
            stopped: list
                .iter()
                .filter(|ticket| ticket.stopped())
                .map(|ticket| ticket.pid)
                .collect(),
        }
    }

    pub(crate) fn open(&self, pid: u32) -> bool {
        !self.lent.contains(&pid) && !self.stopped.contains(&pid)
    }
}

pub(crate) struct View {
    pub(crate) list: Vec<Ticket>,
    gate: Gate,
    pub(crate) holding: Vec<u32>,
    offered: Vec<(u32, u128)>,
    pub(crate) passed: Vec<u32>,
    pub(crate) seats: usize,
    paused: bool,
    night: bool,
}

impl View {
    pub(crate) fn read(dir: &Path) -> Self {
        Self::of(dir, order(dir, tickets(dir)), seats::count())
    }

    pub(crate) fn of(dir: &Path, list: Vec<Ticket>, seats: usize) -> Self {
        let paused = dir.join("paused").exists();
        let list: Vec<Ticket> = list.into_iter().filter(|t| !paused || t.started).collect();
        let holding = holding(dir);
        let offered = offers(dir, &list);
        let (at, pass) = (millis(), pass().as_millis());
        let passed = offered
            .iter()
            .filter(|(pid, since)| !holding.contains(pid) && at.saturating_sub(*since) >= pass)
            .map(|(pid, _)| *pid)
            .collect();
        Self {
            gate: Gate::read(dir, &list),
            offered,
            passed,
            night: night(),
            seats: seats::room(seats, holding.len()),
            holding,
            paused,
            list,
        }
    }

    fn open(&self, me: u32, keep_me: bool) -> Vec<&Ticket> {
        self.list
            .iter()
            .filter(|t| {
                self.gate.open(t.pid)
                    && if t.pid == me {
                        keep_me
                    } else {
                        !self.passed.contains(&t.pid)
                    }
            })
            .collect()
    }

    pub(crate) fn offer(&self, dir: &Path) {
        let folder = dir.join("offered");
        let at = millis();
        for t in &self.list {
            let waits = !self.holding.contains(&t.pid) && self.gate.open(t.pid);
            let offered = waits && (self.passed.contains(&t.pid) || self.admits(t.pid).is_ok());
            let marked = self.offered.iter().any(|(pid, _)| *pid == t.pid);
            let path = folder.join(t.pid.to_string());
            if offered && !marked {
                let _ = write_atomic(&path, &format!("{}\n{at}\n", t.start));
            } else if !offered && marked {
                let _ = fs::remove_file(&path);
            }
        }
        let Ok(entries) = fs::read_dir(&folder) else {
            return;
        };
        for entry in entries.flatten() {
            let gone = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .is_none_or(|pid| !self.list.iter().any(|t| t.pid == pid));
            if gone {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn line<'a>(&self, open: &'a [&'a Ticket], holding: &'a [u32]) -> Line<'a> {
        Line {
            open,
            holding,
            room: self.seats,
        }
    }

    pub(crate) fn admits(&self, me: u32) -> Result<(), Wait> {
        if let Some(&bench) = self.gate.reclaim.first() {
            if bench != me {
                return Err(Wait::Alone(bench));
            }
            return if self.holding.iter().any(|pid| *pid != me) {
                Err(Wait::Clear)
            } else {
                Ok(())
            };
        }
        if let Some(lender) = self.loaned()
            && self.list.iter().any(|t| t.pid == me && t.alone)
        {
            return Err(Wait::Alone(lender));
        }
        let mine = self.list.iter().find(|t| t.pid == me);
        let mut open = self.open(me, true);
        if mine.is_some_and(|t| t.alone) {
            if !self.night
                && mine.is_some_and(|t| rerun(t, &self.holding))
                && open.iter().any(|t| t.pid != me && !t.alone)
            {
                return Err(Wait::Empty);
            }
        } else if !self.night || mine.is_some_and(|t| t.started) {
            let waits = self.night && open.iter().any(|t| rerun(t, &self.holding));
            open.retain(|t| !rerun(t, &self.holding) && (t.started || !waits));
        }
        self.line(&open, &self.holding).admits(me, &self.list)
    }

    fn loaned(&self) -> Option<u32> {
        self.list
            .iter()
            .find(|t| t.alone && self.gate.lent.contains(&t.pid))
            .map(|t| t.pid)
    }

    pub(crate) fn reclaimed(&self, me: u32) -> bool {
        self.gate.reclaim.iter().any(|pid| *pid != me)
    }

    pub(crate) fn reclaimers(&self) -> &[u32] {
        &self.gate.reclaim
    }

    fn waiters(&self, me: u32) -> (Vec<&Ticket>, Vec<u32>) {
        let holding: Vec<u32> = self
            .holding
            .iter()
            .copied()
            .filter(|pid| *pid != me)
            .collect();
        (self.open(me, false), holding)
    }

    pub(crate) fn next_up(&self, me: u32) -> bool {
        let alone = self.list.iter().any(|t| t.pid == me && t.alone);
        let (open, holding) = self.waiters(me);
        let line = self.line(&open, &holding);
        open.iter()
            .filter(|t| !holding.contains(&t.pid) && !(alone && t.alone))
            .any(|t| line.admits(t.pid, &self.list).is_ok())
    }

    fn next_two(&self, me: u32) -> Vec<u32> {
        let (open, holding) = self.waiters(me);
        open.iter()
            .filter(|t| !holding.contains(&t.pid))
            .map(|t| t.pid)
            .take(2)
            .collect()
    }

    pub(crate) fn why(&self, wait: Wait) -> (&'static str, String) {
        match wait {
            Wait::Alone(pid) => {
                let who = self.list.iter().find(|t| t.pid == pid);
                (
                    "alone",
                    format!(
                        "{} runs alone ({})",
                        who.map_or("a job", |t| t.owner.as_str()),
                        who.map_or("bench", |t| t.name.as_str())
                    ),
                )
            }
            Wait::Empty => (
                "alone",
                format!(
                    "re-runs alone: waits for an empty queue or the night ({:02}:00 to {:02}:00)",
                    NIGHT.0, NIGHT.1
                ),
            ),
            Wait::Clear => (
                "alone",
                "runs alone: waits for the other GPU jobs to give way".to_string(),
            ),
            Wait::Seats if self.holding.len() < self.seats => (
                "turn",
                "a GPU lane is free, but jobs ahead of it in line come first: waits its turn"
                    .to_string(),
            ),
            Wait::Seats => (
                "turn",
                if self.seats > 1 {
                    format!("all {} GPU lanes busy: waits its turn", self.seats)
                } else {
                    "the GPU is busy: waits its turn".to_string()
                },
            ),
        }
    }
}

pub(crate) fn wake_next(dir: &Path, me: u32) {
    for pid in View::read(dir).next_two(me) {
        ring_wrapper(dir, pid, RING);
    }
}

pub(crate) fn acquire(
    dir: &Path,
    seats: &mut Seats,
    me: u32,
    quiet: bool,
    check: &mut dyn FnMut() -> Option<Leave>,
    nap: &mut dyn FnMut(Duration),
) -> Result<(), Leave> {
    let mut told = quiet;
    loop {
        if let Some(leave) = check() {
            return Err(leave);
        }
        let view = View::read(dir);
        seats::hold_older(dir);
        view.offer(dir);
        let admitted = view.admits(me);
        let alone = view.list.iter().any(|t| t.pid == me && t.alone);
        if admitted.is_ok() && seats.take(alone) {
            return Ok(());
        }
        if !told && let Err(wait) = admitted {
            let why = if view.paused && !view.list.iter().any(|t| t.pid == me) {
                "paused".to_string()
            } else {
                view.why(wait).1
            };
            say(&format!("pgpu: waiting ({why})"));
            told = true;
        }
        nap(TICK);
    }
}

pub(crate) fn record(who: &str, class: u8, held: &Hold, pid: u32, end: &str) {
    hour::append(
        &state().join("log.tsv"),
        &hour::line(held.from, millis(), who, class, held.waited, pid, end),
    );
}

pub(crate) fn says(path: &Path, word: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|v| v.trim() == word)
}

pub(crate) fn flag(set: bool) -> ExitCode {
    let path = home().join("paused");
    if set {
        let _ = File::create(&path);
        say_out("paused: new jobs hold until resume; started jobs finish");
    } else {
        let _ = fs::remove_file(&path);
        say_out("resumed");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{
        Body, Gate, Mark, Parsed, Ticket, View, clean, holder_of, lent_marked, marked, order,
        parse_ticket, rank, running_body, session, ticket_body, tickets, tickets_in,
    };
    use crate::paths::start_time;
    use crate::seats::holders;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn ticket(class: u8, arrived: u128, pid: u32, owner: &str) -> Ticket {
        Ticket {
            arrived,
            ..Ticket::bare(class, pid, owner)
        }
    }

    fn body(owner: &str, label: &str, started: bool, aged: bool, start: u64) -> String {
        ticket_body(&Body {
            owner,
            label,
            started,
            mark: if aged { Mark::Aged } else { Mark::Plain },
            start,
            name: "clip",
            alone: false,
        })
    }

    #[test]
    fn a_ticket_says_it_started_and_keeps_its_start_time() {
        let dir = test_dir("started");
        fs::create_dir_all(dir.join("tickets")).unwrap();
        let pid = std::process::id();
        let start = start_time(pid).unwrap();
        let path = dir.join("tickets").join(format!("1-10-{pid}"));
        fs::write(&path, body("alpha", "sleep", false, false, start)).unwrap();
        let got = tickets(&dir);
        assert_eq!(got.len(), 1);
        assert!(!got[0].started);
        assert_eq!(got[0].start, start);
        fs::write(&path, body("alpha", "sleep", true, false, start)).unwrap();
        assert!(tickets(&dir)[0].started);
        assert!(parse_ticket("alpha\nsleep\n\n").is_none());
        assert!(parse_ticket("alpha\nsleep\n\nran\n").is_none());
        fs::write(&path, "alpha\nsleep\n\n").unwrap();
        assert!(tickets(&dir).is_empty());
        assert!(!path.exists(), "a three-line ticket is stale");
        fs::write(&path, body("alpha", "sleep", true, false, start + 1)).unwrap();
        assert!(tickets(&dir).is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn a_label_keeps_to_one_line() {
        assert_eq!(clean("sh -c sleep 2\nrealtime"), "sh -c sleep 2 realtime");
        assert_eq!(clean("a\tb\r\nc"), "a b  c");
        assert_eq!(clean("plain ü"), "plain ü");
        let body = body("e4", &clean("x\ny"), true, false, 5);
        assert_eq!(body, "e4\nx y\n\nran\n5\nclip\n\n");
        assert_eq!(
            parse_ticket(&body),
            Some(Parsed {
                owner: "e4".into(),
                label: "x y".into(),
                started: true,
                mark: Mark::Plain,
                start: 5,
                name: Some("clip".into()),
                alone: false,
            })
        );
    }

    #[test]
    fn a_ticket_names_its_class_and_whether_it_runs_alone_and_an_older_one_still_reads() {
        let bench = ticket_body(&Body {
            owner: "pfx",
            label: "bench",
            started: false,
            mark: Mark::Plain,
            start: 7,
            name: "bench",
            alone: true,
        });
        assert_eq!(bench, "pfx\nbench\n\n\n7\nbench\nalone\n");
        let parsed = parse_ticket(&bench).unwrap();
        assert_eq!(
            (parsed.name.as_deref(), parsed.alone),
            (Some("bench"), true)
        );
        let old = parse_ticket("pfx\nrender\naged\nran\n7\n").unwrap();
        assert_eq!((old.name, old.alone, old.mark), (None, false, Mark::Aged));
        assert_eq!(parse_ticket("a\nx\n\n\n7\nwarp\n").unwrap().name, None);
    }

    #[test]
    fn an_aged_ticket_says_so_on_its_third_line_and_ranks_as_clip_but_never_above() {
        let body = body("studio", "pfx bake", false, true, 7);
        assert_eq!(body, "studio\npfx bake\naged\n\n7\nclip\n\n");
        let parsed = parse_ticket(&body).unwrap();
        assert!(parsed.mark == Mark::Aged && !parsed.started);
        assert!(
            parse_ticket("a\nx\n\nran\n5\n").is_some_and(|p| p.mark == Mark::Plain && p.started)
        );
        assert!(parse_ticket("a\nx\nrealtime\nran\n5\n").is_some_and(|p| p.mark == Mark::Plain));
        let marked = |class, mark| Ticket {
            mark,
            ..ticket(class, 0, 1, "a")
        };
        let aged = |class| marked(class, Mark::Aged);
        assert_eq!(aged(3).ranks_as(), 1);
        assert_eq!(aged(2).ranks_as(), 1);
        assert_eq!(aged(1).ranks_as(), 1);
        assert_eq!(aged(0).ranks_as(), 0);
        assert!(aged(3).note().is_some() && aged(2).note().is_some());
        assert!(aged(1).note().is_none() && aged(0).note().is_none());
        for mark in [Mark::Long, Mark::Unpaced] {
            assert_eq!(marked(0, mark).ranks_as(), 1, "{mark:?}");
            assert!(marked(0, mark).note().is_some());
            assert_eq!(marked(2, mark).ranks_as(), 2);
        }
        for class in 0..=3 {
            assert_eq!(ticket(class, 0, 1, "a").ranks_as(), class);
            assert!(ticket(class, 0, 1, "a").note().is_none());
        }
    }

    #[test]
    fn an_aged_truth_or_search_job_takes_turns_with_clips_and_interactive_still_goes_first() {
        let dir = test_dir("aged");
        fs::create_dir_all(dir.join("served")).unwrap();
        fs::create_dir_all(dir.join("turns")).unwrap();
        fs::write(dir.join("served").join("pfx"), "900").unwrap();
        fs::write(dir.join("served").join("manf"), "800").unwrap();
        fs::write(dir.join("served").join("studio"), "100").unwrap();
        let bake = |aged| Ticket {
            mark: if aged { Mark::Aged } else { Mark::Plain },
            ..ticket(2, 5, 4, "studio")
        };
        let search = |aged| Ticket {
            mark: if aged { Mark::Aged } else { Mark::Plain },
            ..ticket(3, 1, 5, "paper")
        };
        let list = |aged| {
            vec![
                ticket(1, 10, 1, "pfx"),
                ticket(1, 20, 2, "manf"),
                ticket(0, 30, 3, "gate"),
                bake(aged),
                search(aged),
            ]
        };
        let pids = |list: Vec<Ticket>| list.iter().map(|t| t.pid).collect::<Vec<_>>();
        assert_eq!(pids(order(&dir, list(false))), vec![3, 2, 1, 4, 5]);
        assert_eq!(pids(order(&dir, list(true))), vec![3, 5, 4, 2, 1]);
        fs::write(dir.join("served").join("studio"), "1000").unwrap();
        fs::write(dir.join("served").join("paper"), "1100").unwrap();
        assert_eq!(pids(order(&dir, list(true))), vec![3, 2, 1, 4, 5]);
        let mut fresh = list(true);
        fresh.retain(|t| t.pid != 4 && t.pid != 5);
        fresh.push(bake(false));
        fresh.push(search(true));
        assert_eq!(pids(order(&dir, fresh)), vec![3, 2, 1, 5, 4]);
    }

    #[test]
    fn a_dead_holder_is_no_holder() {
        let dir = test_dir("holder");
        let me = std::process::id();
        let start = start_time(me).unwrap();
        assert_eq!(holder_of(&running_body("a", "x", 7, 9)), Some((7, 9)));
        assert_eq!(holder_of("a\nx\n7\n"), None);
        assert_eq!(holder_of("a\nx\nrealtime\n7\n"), None);
        fs::write(dir.join("running.1"), running_body("a", "x", me, start)).unwrap();
        let found = holders(&dir);
        assert_eq!((found[0].label.as_str(), found[0].pid), ("x", me));
        fs::write(dir.join("running.1"), running_body("a", "x", me, start + 1)).unwrap();
        assert!(holders(&dir).is_empty());
    }

    #[test]
    fn a_session_takes_turns_between_its_own_tickets() {
        let dir = test_dir("per-ticket");
        fs::create_dir_all(dir.join("served")).unwrap();
        fs::create_dir_all(dir.join("turns")).unwrap();
        fs::write(dir.join("served").join("pengine"), "500").unwrap();
        fs::write(dir.join("served").join("other"), "400").unwrap();
        let list = || {
            vec![
                ticket(0, 10, 1, "pengine"),
                ticket(0, 20, 2, "pengine"),
                ticket(0, 30, 3, "other"),
                ticket(1, 5, 4, "pengine"),
            ]
        };
        let pids = |list: Vec<Ticket>| list.iter().map(|t| t.pid).collect::<Vec<_>>();
        assert_eq!(pids(order(&dir, list())), vec![3, 1, 2, 4]);
        let turned = pids(rank(&dir, list(), Some(("pengine", 1, 1000))));
        assert_eq!(turned, vec![3, 2, 1, 4]);
        fs::write(dir.join("served").join("pengine"), "1000").unwrap();
        fs::write(dir.join("turns").join("1.served"), "1000").unwrap();
        assert_eq!(pids(order(&dir, list())), turned);
        let back = pids(rank(&dir, list(), Some(("pengine", 2, 2000))));
        assert_eq!(back, vec![3, 1, 2, 4]);
        fs::write(dir.join("served").join("pengine"), "2000").unwrap();
        fs::write(dir.join("turns").join("2.served"), "2000").unwrap();
        assert_eq!(pids(order(&dir, list())), back);
    }

    fn view(dir: &Path) -> View {
        View::of(dir, order(dir, tickets(dir)), 1)
    }

    fn next_up_here(dir: &Path, me: u32) -> bool {
        view(dir).next_up(me)
    }

    fn admits_here(dir: &Path, me: u32) -> bool {
        view(dir).admits(me).is_ok()
    }

    #[test]
    fn a_holder_giving_way_rings_the_two_waiters_next_after_it() {
        let dir = test_dir("next-two");
        for sub in ["tickets", "served", "turns"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let mut jobs = [spawn_sleeper(), spawn_sleeper(), spawn_sleeper()];
        let pids: Vec<u32> = jobs.iter().map(|j| j.id()).collect();
        let (a, b, c) = (pids[0], pids[1], pids[2]);
        for (pid, arrived, owner, started) in [
            (a, 10, "alpha", true),
            (b, 20, "beta", false),
            (c, 30, "gamma", false),
        ] {
            fs::write(
                dir.join("tickets").join(format!("1-{arrived}-{pid}")),
                body(owner, "render", started, false, start_time(pid).unwrap()),
            )
            .unwrap();
        }
        fs::write(
            dir.join("running"),
            running_body("alpha", "render", a, start_time(a).unwrap()),
        )
        .unwrap();
        fs::write(dir.join("served").join("alpha"), "1000").unwrap();
        fs::write(dir.join("served").join("beta"), "2000").unwrap();
        fs::write(dir.join("served").join("gamma"), "500").unwrap();
        assert_eq!(view(&dir).next_two(a), vec![c, b]);
        assert_eq!(view(&dir).next_two(c), vec![b], "the holder is not rung");
        for job in &mut jobs {
            let _ = job.kill();
            let _ = job.wait();
        }
    }

    fn spawn_sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_lent_ticket_is_passed_over_and_no_one_else_means_nothing_to_lend_to() {
        let dir = test_dir("lent");
        for sub in ["tickets", "served", "turns", "lent"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let mut lender = spawn_sleeper();
        let mut waiter = spawn_sleeper();
        let (a, b) = (lender.id(), waiter.id());
        let (a_start, b_start) = (start_time(a).unwrap(), start_time(b).unwrap());
        fs::write(
            dir.join("tickets").join(format!("1-10-{a}")),
            body("alpha", "cargo test", true, false, a_start),
        )
        .unwrap();
        assert!(!next_up_here(&dir, a), "no one waits");
        assert!(admits_here(&dir, a));
        let b_ticket = dir.join("tickets").join(format!("1-20-{b}"));
        fs::write(&b_ticket, body("beta", "render", false, false, b_start)).unwrap();
        assert!(next_up_here(&dir, a));
        assert!(next_up_here(&dir, b), "the lender is not lent yet");
        assert!(!admits_here(&dir, b));
        let mark = dir.join("lent").join(a.to_string());
        fs::write(&mark, format!("{a_start}\n")).unwrap();
        assert_eq!(lent_marked(&dir, &tickets(&dir)), vec![a]);
        assert!(admits_here(&dir, b), "the lent ticket is passed");
        assert!(!next_up_here(&dir, b), "nothing else could start");
        fs::write(&mark, format!("{}\n", a_start + 1)).unwrap();
        assert!(
            lent_marked(&dir, &tickets(&dir)).is_empty(),
            "a mark of another process with that pid"
        );
        fs::write(&mark, format!("{a_start}\n")).unwrap();
        fs::write(dir.join("paused"), "").unwrap();
        assert!(!next_up_here(&dir, a), "a paused queue starts no new job");
        fs::remove_file(dir.join("paused")).unwrap();
        fs::remove_file(&b_ticket).unwrap();
        assert!(!next_up_here(&dir, a));
        let _ = lender.kill();
        let _ = waiter.kill();
        let _ = lender.wait();
        let _ = waiter.wait();
    }

    #[test]
    fn as_rule() {
        test_dir("as");
        assert_eq!(session("we-ird ü\"x"), "we_ird___x");
        assert_eq!(session("Az9._"), "Az9._");
        for name in ["", ".", ".."] {
            assert_eq!(session(name), "unknown", "{name:?}");
        }
        assert_eq!(session("..."), "...");
        assert_eq!(session("/"), "_");
        assert_eq!(session(&"a".repeat(300)), "a".repeat(255));
        assert_eq!(session(&"ü".repeat(200)), "_".repeat(200));
        assert_eq!(session(&"ü".repeat(300)).len(), 255);
    }

    fn stat_line(pid: u32, state: char, start: u64) -> String {
        format!(
            "{pid} (sh) {state} 1 {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 1000 10\n"
        )
    }

    #[test]
    fn a_ticket_goes_only_on_a_clear_answer_and_keeps_its_wrappers_state() {
        let dir = test_dir("clear-answer");
        let proc_root = dir.join("proc");
        fs::create_dir_all(dir.join("tickets")).unwrap();
        let ticket = |pid: u32, start: u64| {
            let path = dir.join("tickets").join(format!("1-{pid}-{pid}"));
            fs::write(&path, body("a", "x", false, false, start)).unwrap();
            path
        };
        let stale = |pid: u32, body: &str| {
            let path = dir.join("tickets").join(format!("1-{pid}-{pid}"));
            fs::write(&path, body).unwrap();
            path
        };
        let proc_of = |pid: u32, stat: Option<&str>| {
            let folder = proc_root.join(pid.to_string());
            fs::create_dir_all(&folder).unwrap();
            match stat {
                Some(stat) => fs::write(folder.join("stat"), stat).unwrap(),
                None => fs::create_dir_all(folder.join("stat")).unwrap(),
            }
        };
        let same = ticket(11, 500);
        proc_of(11, Some(&stat_line(11, 'S', 500)));
        let stopped = ticket(12, 600);
        proc_of(12, Some(&stat_line(12, 'T', 600)));
        let other = ticket(13, 700);
        proc_of(13, Some(&stat_line(13, 'S', 701)));
        let absent = ticket(14, 800);
        let unreadable = ticket(15, 900);
        proc_of(15, None);
        let garbled = ticket(16, 1000);
        proc_of(16, Some("16 (sh"));
        let no_start = stale(17, "a\nx\n\n\n");
        proc_of(17, Some(&stat_line(17, 't', 5)));
        let absent_no_start = stale(18, "a\nx\n\nran\n");
        let mut got: Vec<(u32, Option<char>, bool)> = tickets_in(&dir, &proc_root)
            .iter()
            .map(|t| (t.pid, t.state, t.stopped()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                (11, Some('S'), false),
                (12, Some('T'), true),
                (15, None, false),
                (16, None, false),
            ]
        );
        for kept in [&same, &stopped, &unreadable, &garbled] {
            assert!(kept.exists(), "{}", kept.display());
        }
        for dropped in [&other, &absent, &no_start, &absent_no_start] {
            assert!(!dropped.exists(), "{}", dropped.display());
        }
    }

    #[test]
    fn a_mark_counts_only_for_the_process_whose_start_it_names() {
        let dir = test_dir("marked");
        fs::create_dir_all(dir.join("lent")).unwrap();
        let started = |pid: u32, start: u64| Ticket {
            start,
            ..ticket(1, 1, pid, "a")
        };
        let list = vec![
            started(1, 100),
            started(2, 200),
            started(3, 300),
            started(4, 400),
            started(5, 500),
        ];
        let mark =
            |pid: u32, body: &str| fs::write(dir.join("lent").join(pid.to_string()), body).unwrap();
        mark(1, "100\n");
        mark(2, "201\n");
        mark(3, "\n");
        mark(4, "400\n");
        assert_eq!(marked(&dir, "lent", &list), vec![1, 4]);
        assert_eq!(lent_marked(&dir, &list), vec![1, 4]);
        fs::remove_file(dir.join("lent").join("1")).unwrap();
        assert_eq!(lent_marked(&dir, &list), vec![4]);
    }

    #[test]
    fn a_stopped_wrapper_is_closed_at_the_gate() {
        let dir = test_dir("gate-stopped");
        let with = |pid: u32, state: Option<char>| Ticket {
            state,
            ..ticket(1, 1, pid, "a")
        };
        let list = vec![
            with(1, Some('T')),
            with(2, Some('t')),
            with(3, Some('S')),
            with(4, Some('R')),
            with(5, None),
        ];
        let gate = Gate::read(&dir, &list);
        let open: Vec<u32> = list
            .iter()
            .filter(|t| gate.open(t.pid))
            .map(|t| t.pid)
            .collect();
        assert_eq!(open, vec![3, 4, 5]);
    }

    #[test]
    fn order_class_then_served_then_arrival() {
        let dir = test_dir("order");
        fs::create_dir_all(dir.join("served")).unwrap();
        fs::write(dir.join("served").join("alpha"), "100").unwrap();
        fs::write(dir.join("served").join("beta"), "50").unwrap();
        fs::write(dir.join("served").join("desk"), "999").unwrap();
        let list = vec![
            ticket(1, 30, 1, "alpha"),
            ticket(0, 100, 2, "desk"),
            ticket(1, 10, 3, "alpha"),
            ticket(1, 40, 4, "gamma"),
            ticket(1, 1, 5, "beta"),
            ticket(2, 1, 6, "late"),
        ];
        let ordered = order(&dir, list);
        let got: Vec<(u8, &str, u128)> = ordered
            .iter()
            .map(|t| (t.class, t.owner.as_str(), t.arrived))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, "desk", 100),
                (1, "gamma", 40),
                (1, "beta", 1),
                (1, "alpha", 10),
                (1, "alpha", 30),
                (2, "late", 1),
            ]
        );
    }
}
