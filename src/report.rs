use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use crate::gpu::busy;
use crate::hour;
use crate::outside;
use crate::pace;
use crate::paths::{home, millis, now, state, turn_file};
use crate::queue::{Ticket, View, lent_marked, order, says, tickets};
use crate::seats::{self, holders};
use crate::usage;

pub(crate) fn span(ms: u128) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

unsafe extern "C" {
    fn tzset();
}

fn offset_at(secs: i64) -> i128 {
    let time = secs as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { tzset() };
    if unsafe { libc::localtime_r(&time, &mut local) }.is_null() {
        return 0;
    }
    i128::from(local.tm_gmtoff) * 1000
}

fn offset_ms() -> i128 {
    offset_at((millis() / 1000) as i64)
}

pub(crate) fn local_hour() -> u8 {
    u8::try_from((millis() as i128 + offset_ms()).rem_euclid(86_400_000) / 3_600_000).unwrap_or(0)
}

pub(crate) fn civil(day: i128) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

pub(crate) fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Day {
    day: String,
    held: u128,
    active: u128,
    holds: usize,
    waits: [u128; 3],
    sessions: Vec<(String, u128)>,
}

pub(crate) fn stats(days: u128, pace_log: bool, json: bool) -> ExitCode {
    if pace_log {
        let body = fs::read_to_string(state().join("pace.tsv")).unwrap_or_default();
        let since = millis().saturating_sub(days * 86_400_000);
        let text = pace::render(&pace::sessions(&body, since), days, json);
        let _ = write!(std::io::stdout().lock(), "{text}");
        return ExitCode::SUCCESS;
    }
    let body = fs::read_to_string(state().join("log.tsv")).unwrap_or_default();
    let since = millis().saturating_sub(days * 86_400_000);
    let offset = offset_ms();
    let mut by_day: std::collections::BTreeMap<i128, Vec<(u128, u128, String, u128)>> =
        Default::default();
    for line in body.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let (Some(from), Some(to), Some(who), Some(waited)) = (
            f.first().and_then(|v| v.parse::<u128>().ok()),
            f.get(1).and_then(|v| v.parse::<u128>().ok()),
            f.get(2),
            f.get(4).and_then(|v| v.parse::<u128>().ok()),
        ) else {
            continue;
        };
        if to < since {
            continue;
        }
        let day = (from as i128 + offset).div_euclid(86_400_000);
        by_day
            .entry(day)
            .or_default()
            .push((from, to, who.to_string(), waited));
    }
    let rows: Vec<Day> = by_day
        .into_iter()
        .map(|(day, holds)| {
            let held: u128 = holds.iter().map(|h| h.1.saturating_sub(h.0)).sum();
            let first = holds.iter().map(|h| h.0).min().unwrap_or(0);
            let last = holds.iter().map(|h| h.1).max().unwrap_or(0);
            let mut waits: Vec<u128> = holds.iter().map(|h| h.3).collect();
            waits.sort();
            let pick = |q: f64| pace::pick(&waits, q);
            let mut share: std::collections::BTreeMap<&str, u128> = Default::default();
            for h in &holds {
                *share.entry(h.2.as_str()).or_default() += h.1.saturating_sub(h.0);
            }
            let mut sessions: Vec<(String, u128)> = share
                .into_iter()
                .map(|(who, ms)| (who.to_string(), ms))
                .collect();
            sessions.sort_by_key(|s| std::cmp::Reverse(s.1));
            Day {
                day: civil(day),
                held,
                active: last.saturating_sub(first).max(1),
                holds: holds.len(),
                waits: [pick(0.5), pick(0.9), pick(1.0)],
                sessions,
            }
        })
        .collect();
    let mut out = std::io::stdout().lock();
    if json {
        let body: Vec<String> = rows
            .iter()
            .map(|d| {
                let sessions: Vec<String> = d
                    .sessions
                    .iter()
                    .map(|(who, ms)| {
                        format!(
                            "{{\"as\":{},\"held_ms\":{ms},\"share\":{}}}",
                            quote(who),
                            ms * 100 / d.held.max(1)
                        )
                    })
                    .collect();
                format!(
                    "{{\"day\":\"{}\",\"held_ms\":{},\"active_ms\":{},\"busy\":{},\"holds\":{},\
                     \"wait_ms\":{{\"p50\":{},\"p90\":{},\"max\":{}}},\"sessions\":[{}]}}",
                    d.day,
                    d.held,
                    d.active,
                    d.held * 100 / d.active,
                    d.holds,
                    d.waits[0],
                    d.waits[1],
                    d.waits[2],
                    sessions.join(",")
                )
            })
            .collect();
        let killed: Vec<String> = killed(since).iter().map(Killed::json).collect();
        let benches: Vec<String> = benches(since).iter().map(Bench::json).collect();
        let _ = writeln!(
            out,
            "{{\"v\":1,\"days\":[{}],\"killed\":[{}],\"benches\":[{}]}}",
            body.join(","),
            killed.join(","),
            benches.join(",")
        );
        return ExitCode::SUCCESS;
    }
    let stopped = killed(since);
    if rows.is_empty() {
        let _ = writeln!(out, "no GPU holds logged in the last {days} days");
    }
    for job in &stopped {
        let _ = writeln!(out, "{}", job.text(offset));
    }
    for bench in benches(since) {
        let _ = writeln!(out, "{}", bench.text(offset));
    }
    if rows.is_empty() {
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(
        out,
        "{:<10}  {:>7}  {:>7}  {:>4}  {:>5}  {:>22}  sessions",
        "day", "held", "active", "busy", "holds", "wait p50 / p90 / max"
    );
    for d in rows {
        let sessions: Vec<String> = d
            .sessions
            .iter()
            .map(|(who, ms)| format!("{who} {}%", ms * 100 / d.held.max(1)))
            .collect();
        let _ = writeln!(
            out,
            "{:<10}  {:>7}  {:>7}  {:>3}%  {:>5}  {:>22}  {}",
            d.day,
            span(d.held),
            span(d.active),
            d.held * 100 / d.active,
            d.holds,
            format!(
                "{} / {} / {}",
                span(d.waits[0]),
                span(d.waits[1]),
                span(d.waits[2])
            ),
            sessions.join(", ")
        );
    }
    ExitCode::SUCCESS
}

#[derive(Debug, PartialEq)]
struct Killed {
    at: u128,
    who: String,
    class: String,
    pid: u32,
    budget_ms: u128,
    ran_ms: u128,
    why: String,
    label: String,
    memory: Option<u64>,
}

impl Killed {
    fn parse(line: &str) -> Option<Self> {
        let mut f = line.splitn(9, '\t');
        Some(Self {
            at: f.next()?.parse().ok()?,
            who: f.next()?.to_string(),
            class: f.next()?.to_string(),
            pid: f.next()?.parse().ok()?,
            budget_ms: f.next()?.parse().ok()?,
            ran_ms: f.next()?.parse().ok()?,
            why: f.next()?.to_string(),
            label: f.next().unwrap_or("").to_string(),
            memory: f.next().and_then(|bytes| bytes.trim().parse().ok()),
        })
    }

    fn json(&self) -> String {
        format!(
            "{{\"at\":{},\"as\":{},\"class\":{},\"pid\":{},\"budget_ms\":{},\"ran_ms\":{},\"memory_bytes\":{},\"why\":{},\"label\":{}}}",
            self.at,
            quote(&self.who),
            quote(&self.class),
            self.pid,
            self.budget_ms,
            self.ran_ms,
            self.memory
                .map_or("null".to_string(), |bytes| bytes.to_string()),
            quote(&self.why),
            quote(&self.label)
        )
    }

    fn text(&self, offset: i128) -> String {
        let local = self.at as i128 + offset;
        let day = local.div_euclid(86_400_000);
        let secs = local.rem_euclid(86_400_000) / 1000;
        format!(
            "stopped  {} {:02}:{:02}  {}  {}  {} after {} (budget {})  {}",
            civil(day),
            secs / 3600,
            secs % 3600 / 60,
            self.who,
            self.class,
            match self.why.as_str() {
                "stall" => "stalled".to_string(),
                "memory" => match self.memory {
                    Some(bytes) => format!("out of memory at {}", crate::budget::size(bytes)),
                    None => "out of memory".to_string(),
                },
                _ => "over twice its budget".to_string(),
            },
            span(self.ran_ms),
            span(self.budget_ms),
            short(&self.label)
        )
    }
}

#[derive(Debug, PartialEq)]
struct Bench {
    at: u128,
    who: String,
    class: String,
    pid: u32,
    ran_ms: u128,
    shared: u32,
    busy: Option<u32>,
    alone: String,
    label: String,
}

impl Bench {
    fn parse(line: &str) -> Option<Self> {
        let mut f = line.splitn(9, '\t');
        Some(Self {
            at: f.next()?.parse().ok()?,
            who: f.next()?.to_string(),
            class: f.next()?.to_string(),
            pid: f.next()?.parse().ok()?,
            ran_ms: f.next()?.parse().ok()?,
            shared: f.next()?.parse().ok()?,
            busy: f.next()?.parse().ok(),
            alone: f.next()?.to_string(),
            label: f.next().unwrap_or("").to_string(),
        })
    }

    fn json(&self) -> String {
        format!(
            "{{\"at\":{},\"as\":{},\"class\":{},\"pid\":{},\"ran_ms\":{},\"shared\":{},\"gpu_busy\":{},\"alone\":{},\"label\":{}}}",
            self.at,
            quote(&self.who),
            quote(&self.class),
            self.pid,
            self.ran_ms,
            self.shared,
            self.busy
                .map_or("null".to_string(), |busy| busy.to_string()),
            if self.alone.is_empty() {
                "null".to_string()
            } else {
                quote(&self.alone)
            },
            quote(&self.label)
        )
    }

    fn text(&self, offset: i128) -> String {
        let local = self.at as i128 + offset;
        let day = local.div_euclid(86_400_000);
        let secs = local.rem_euclid(86_400_000) / 1000;
        let beside = if !self.alone.is_empty() {
            format!("alone ({})", self.alone)
        } else {
            match self.shared {
                0 => "with the card to itself".to_string(),
                1 => "beside 1 other job".to_string(),
                n => format!("beside {n} other jobs"),
            }
        };
        format!(
            "bench    {} {:02}:{:02}  {}  {}  ran {} {beside}{}  {}",
            civil(day),
            secs / 3600,
            secs % 3600 / 60,
            self.who,
            self.class,
            span(self.ran_ms),
            self.busy
                .map_or(String::new(), |busy| format!(", GPU {busy}% busy")),
            short(&self.label)
        )
    }
}

fn benches(since: u128) -> Vec<Bench> {
    fs::read_to_string(state().join("benches.tsv"))
        .unwrap_or_default()
        .lines()
        .filter_map(Bench::parse)
        .filter(|b| b.at >= since)
        .collect()
}

fn killed(since: u128) -> Vec<Killed> {
    fs::read_to_string(state().join("killed.tsv"))
        .unwrap_or_default()
        .lines()
        .filter_map(Killed::parse)
        .filter(|k| k.at >= since)
        .collect()
}

fn vram_json() -> String {
    usage::card().map_or("null".to_string(), |card| {
        format!(
            "{{\"used_mib\":{},\"total_mib\":{}}}",
            card.used / (1024 * 1024),
            card.total / (1024 * 1024)
        )
    })
}

fn pace_of(dir: &Path, pid: u32, running: bool) -> Option<pace::Summary> {
    let entries = pace::ring(&pace::ring_path(dir, pid));
    let open = running.then(|| pace::last_mark(dir, pid)).flatten();
    pace::summary(&entries, now(), open)
}

type Lanes = BTreeMap<u32, Option<outside::Lane>>;

fn lanes(dir: &Path, rows: &[Job]) -> Lanes {
    let jobs: Vec<(u32, u64)> = rows
        .iter()
        .map(|job| (job.ticket.pid, job.ticket.start))
        .collect();
    outside::lanes(dir, &jobs)
}

struct Ran {
    ran_ms: u128,
    budget_ms: u128,
    frozen_ms: Option<u128>,
    idle_ms: Option<u128>,
}

impl Ran {
    fn over_ms(&self) -> Option<u128> {
        self.ran_ms
            .checked_sub(self.budget_ms)
            .filter(|over| *over > 0)
    }
}

fn ran_of(dir: &Path, pid: u32) -> Option<Ran> {
    let body = fs::read_to_string(turn_file(dir, pid, ".ran")).ok()?;
    let mut fields = body.split_whitespace().map(|v| v.parse::<u128>().ok());
    Some(Ran {
        ran_ms: fields.next()??,
        budget_ms: fields.next()??,
        frozen_ms: fields.nth(1).flatten(),
        idle_ms: fields.next().flatten(),
    })
}

const IDLING: u128 = 3000;

fn idle_bench(dir: &Path, t: &Ticket, running: bool) -> Option<u128> {
    if !t.alone || !running {
        return None;
    }
    ran_of(dir, t.pid)?.idle_ms.filter(|ms| *ms >= IDLING)
}

fn red(text: &str) -> String {
    let style = anstyle::AnsiColor::Red.on_default().bold();
    format!("{style}{text}{style:#}")
}

fn notes(dir: &Path, t: &Ticket, running: bool, lanes: &Lanes) -> String {
    let pid = t.pid;
    let mut parts = Vec::new();
    if let Some(over) =
        ran_of(dir, pid).and_then(|ran| ran.over_ms().map(|over| (over, ran.budget_ms)))
    {
        parts.push(red(&format!(
            "over its budget by {} (budget {})",
            span(over.0),
            crate::budget::span(std::time::Duration::from_millis(
                u64::try_from(over.1).unwrap_or(u64::MAX)
            ))
        )));
    }
    if t.alone {
        parts.push("runs alone".to_string());
    }
    if let Some(idle) = idle_bench(dir, t, running) {
        parts.push(format!(
            "idling the card: under 5% of the GPU for {}",
            span(idle)
        ));
    }
    if let Some(frozen) = ran_of(dir, pid)
        .and_then(|ran| ran.frozen_ms)
        .filter(|ms| *ms >= 1000)
    {
        parts.push(format!("frozen or waiting {}", span(frozen)));
    }
    if let Some(note) = t.note() {
        parts.push(note.to_string());
    }
    if let Some(note) = pace_of(dir, pid, running).and_then(|s| s.note()) {
        parts.push(note);
    }
    if let Some(lane) = lanes.get(&pid).and_then(Option::as_ref) {
        parts.push(format!("lane {}", lane.text()));
    }
    parts.iter().map(|p| format!("  · {p}")).collect()
}

struct Job {
    ticket: Ticket,
    running: bool,
    state: &'static str,
    waits_for: Option<&'static str>,
    why: Option<String>,
}

fn jobs(dir: &Path, holding: &[u32]) -> Vec<Job> {
    let paused = dir.join("paused").exists();
    let mut list = order(dir, tickets(dir));
    let view = View::of(dir, list.clone(), seats::count());
    list.sort_by_key(|t| !holding.contains(&t.pid));
    let lent = lent_marked(dir, &list);
    list.into_iter()
        .map(|t| {
            let running = holding.contains(&t.pid);
            let lent = !running && lent.contains(&t.pid);
            let state = if running {
                "running"
            } else if lent {
                "lent"
            } else if says(&turn_file(dir, t.pid, ""), "wait") {
                "yielded"
            } else {
                "waiting"
            };
            let (waits_for, why) = if running || lent {
                (None, None)
            } else if t.stopped() {
                (
                    Some("stopped"),
                    Some("stopped: passed over until it continues".to_string()),
                )
            } else if paused && !t.started {
                (
                    Some("paused"),
                    Some("the queue is paused: new jobs hold until resume".to_string()),
                )
            } else if let Err(wait) = view.admits(t.pid) {
                let (word, text) = view.why(wait);
                (Some(word), Some(text))
            } else {
                (
                    Some("turn"),
                    Some("a GPU lane is free for it: it takes it at its next look".to_string()),
                )
            };
            Job {
                ticket: t,
                running,
                state,
                waits_for,
                why,
            }
        })
        .collect()
}

fn snapshot(dir: &Path) -> String {
    let held = holders(dir);
    let holding: Vec<u32> = held.iter().map(|h| h.pid).collect();
    let paused = dir.join("paused").exists();
    let rows = jobs(dir, &holding);
    let lanes = lanes(dir, &rows);
    let jobs: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(position, job)| {
            let t = &job.ticket;
            let class = &t.name;
            let state = job.state;
            let since = held
                .iter()
                .find(|h| job.running && h.pid == t.pid)
                .and_then(|h| h.since)
                .map(|ms| format!(",\"since\":{ms}"))
                .unwrap_or_default();
            let waits_for = job
                .waits_for
                .map_or("null".to_string(), |why| format!("\"{why}\""));
            let why = job.why.as_deref().map_or("null".to_string(), quote);
            let ran = ran_of(dir, t.pid);
            let budget = format!(
                "\"alone\":{},\"budget_ms\":{},\"ran_ms\":{},\"over_ms\":{},\"frozen_ms\":{},\"idle_ms\":{}",
                t.alone,
                ran.as_ref()
                    .map_or("null".to_string(), |r| r.budget_ms.to_string()),
                ran.as_ref().map_or("null".to_string(), |r| r.ran_ms.to_string()),
                ran.as_ref()
                    .and_then(Ran::over_ms)
                    .map_or("null".to_string(), |ms| ms.to_string()),
                ran.as_ref()
                    .and_then(|r| r.frozen_ms)
                    .map_or("null".to_string(), |ms| ms.to_string()),
                ran.as_ref()
                    .and_then(|r| r.idle_ms)
                    .map_or("null".to_string(), |ms| ms.to_string())
            );
            let pace = pace_of(dir, t.pid, job.running).map_or("null".to_string(), |s| s.json());
            let lane = outside::lane_json(lanes.get(&t.pid).and_then(Option::as_ref));
            format!(
                "{{\"pid\":{},\"state\":\"{state}\",\"position\":{position},\"class\":\"{class}\",\
                 \"as\":{},\"label\":{},\"realtime\":false,\"arrived\":{}{since},\"waits_for\":{waits_for},\
                 \"why\":{why},{budget},\"pace\":{pace},\"lane\":{lane}}}",
                t.pid,
                quote(&t.owner),
                quote(&t.label),
                t.arrived / 1_000_000
            )
        })
        .collect();
    let gpu_busy = busy().map_or("null".to_string(), |v| v.to_string());
    let vram = vram_json();
    let running: Vec<(u128, u32)> = held
        .iter()
        .filter_map(|h| h.since.map(|since| (since, h.pid)))
        .collect();
    let hour = hour::cached_json(dir, &state().join("log.tsv"), &running, millis());
    let outside = outside::json(&outside::outside(dir));
    format!(
        "{{\"v\":1,\"paused\":{},\"gpu_busy\":{gpu_busy},\"play\":null,\"away\":null,\"away_reason\":null,\"desk\":null,\"vram\":{vram},\"jobs\":[{}],\"hour\":{hour},\"outside\":{outside}}}",
        paused,
        jobs.join(",")
    )
}

fn short(label: &str) -> String {
    label.chars().take(80).collect()
}

pub(crate) fn status(json: bool) -> ExitCode {
    let dir = home();
    if json {
        let _ = writeln!(std::io::stdout().lock(), "{}", snapshot(&dir));
        return ExitCode::SUCCESS;
    }
    let mut out = anstream::stdout().lock();
    let held = holders(&dir);
    let holding: Vec<u32> = held.iter().map(|h| h.pid).collect();
    let rows = jobs(&dir, &holding);
    let lanes = lanes(&dir, &rows);
    if dir.join("paused").exists() {
        let _ = writeln!(out, "paused");
    }
    for job in rows.iter().filter(|job| job.running) {
        let t = &job.ticket;
        let label = held
            .iter()
            .find(|h| h.pid == t.pid)
            .map_or(t.label.as_str(), |h| h.label.as_str());
        let _ = writeln!(
            out,
            "running  {}  {}{}{}",
            t.owner,
            label,
            if t.stopped() {
                "  (stopped: it holds the GPU and the queue waits until it continues)"
            } else {
                ""
            },
            notes(&dir, t, true, &lanes)
        );
    }
    if !rows.iter().any(|job| job.running) {
        let _ = writeln!(out, "running  nothing");
    }
    for job in rows.iter().filter(|job| job.state == "lent") {
        let t = &job.ticket;
        let _ = writeln!(
            out,
            "lent     {}  {}  ({}){}",
            t.owner,
            short(&t.label),
            if t.alone {
                "a job that runs alone, idling the card: lent its lanes until it uses the GPU, then freezes the jobs it lent them to"
            } else {
                "not using the GPU; lent it to the next job"
            },
            notes(&dir, t, false, &lanes)
        );
    }
    if let Some(v) = busy() {
        let _ = writeln!(out, "gpu      {v}% busy");
    }
    for job in rows
        .iter()
        .filter(|job| !job.running && job.state != "lent")
    {
        let t = &job.ticket;
        let why = job
            .why
            .as_deref()
            .map(|why| format!("  ({why})"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "waiting  {:<12} {:<14} {}{why}{}",
            t.name,
            t.owner,
            short(&t.label),
            notes(&dir, t, false, &lanes)
        );
    }
    for row in outside::outside(&dir) {
        let _ = writeln!(out, "{}", row.text());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{civil, offset_at, quote};
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn quote_escapes_and_keeps_utf8() {
        test_dir("quote");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(quote("a\nb"), "\"a\\nb\"");
        assert_eq!(quote("a\tb"), "\"a\\tb\"");
        assert_eq!(quote("\u{1}"), "\"\\u0001\"");
        assert_eq!(quote("ü"), "\"ü\"");
    }

    const WINTER: i64 = 1_767_225_600;
    const SUMMER: i64 = 1_782_864_000;

    #[test]
    fn the_local_offset_follows_tz() {
        if std::env::var("PGPU_TZ_CHILD").is_err() {
            for (tz, winter, summer) in [
                ("EET-2EEST,M3.5.0/3,M10.5.0/4", 2, 3),
                ("UTC0", 0, 0),
                ("<-0330>3:30", -3, -3),
                ("EST5EDT,M3.2.0,M11.1.0", -5, -4),
            ] {
                let out = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "report::tests::the_local_offset_follows_tz"])
                    .env("PGPU_TZ_CHILD", format!("{winter} {summer}"))
                    .env("TZ", tz)
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{tz}: {}",
                    String::from_utf8_lossy(&out.stdout)
                );
            }
            return;
        }
        let want: Vec<i128> = std::env::var("PGPU_TZ_CHILD")
            .unwrap()
            .split(' ')
            .map(|hours| hours.parse().unwrap())
            .collect();
        let tz = std::env::var("TZ").unwrap();
        let half = if tz.starts_with("<-0330>") {
            -1_800_000
        } else {
            0
        };
        assert_eq!(offset_at(WINTER), want[0] * 3_600_000 + half, "{tz}");
        assert_eq!(offset_at(SUMMER), want[1] * 3_600_000 + half, "{tz}");
    }

    #[test]
    fn a_snapshot_of_a_missing_queue_is_empty() {
        let dir = test_dir("snapshot").join("missing");
        let json = super::snapshot(&dir);
        assert!(json.contains("\"jobs\":[]"), "{json}");
        assert!(!dir.exists());
    }

    #[test]
    fn civil_dates() {
        test_dir("civil");
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(11016), "2000-02-29");
        assert_eq!(civil(20726), "2026-09-30");
    }
}
