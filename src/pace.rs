use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::paths::{turn_file, write_atomic};
use crate::wrapper::Act;

const RING: usize = 64;

const STATUS_WINDOW_NS: u128 = 10_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Report {
    pub(crate) work_ms: Option<f64>,
    pub(crate) longest_ms: Option<f64>,
}

fn ms_value(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

pub(crate) fn report(args: &[String]) -> Report {
    let mut out = Report::default();
    let mut i = 0;
    while i < args.len() {
        let (flag, inline) = match args[i].split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (args[i].as_str(), None),
        };
        let slot = match flag {
            "--work-ms" => &mut out.work_ms,
            "--longest-ms" => &mut out.longest_ms,
            _ => {
                i += 1;
                continue;
            }
        };
        match inline {
            Some(value) => {
                if let Some(v) = ms_value(value) {
                    *slot = Some(v);
                }
                i += 1;
            }
            None => match args.get(i + 1).and_then(|v| ms_value(v)) {
                Some(v) => {
                    *slot = Some(v);
                    i += 2;
                }
                None => i += 1,
            },
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Entry {
    pub(crate) at: u128,
    pub(crate) work_ms: Option<f64>,
    pub(crate) longest_ms: Option<f64>,
    pub(crate) gap_ms: Option<f64>,
    pub(crate) frozen_ms: Option<u64>,
}

impl Entry {
    fn reported(&self) -> bool {
        self.work_ms.is_some() || self.longest_ms.is_some()
    }
}

fn cell(value: Option<f64>) -> String {
    value.map_or("-".to_string(), |v| {
        format!("{}", (v * 1000.0).round() / 1000.0)
    })
}

fn entry_line(entry: &Entry) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        entry.at,
        cell(entry.work_ms),
        cell(entry.longest_ms),
        cell(entry.gap_ms),
        entry.frozen_ms.map_or("-".to_string(), |v| v.to_string())
    )
}

fn parse_ring(body: &str) -> Vec<Entry> {
    body.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let at = fields.next()?.trim().parse::<u128>().ok()?;
            let mut next = || fields.next().and_then(ms_value);
            let work_ms = next();
            let longest_ms = next();
            let gap_ms = next();
            let frozen_ms = fields.next().and_then(|t| t.trim().parse::<u64>().ok());
            Some(Entry {
                at,
                work_ms,
                longest_ms,
                gap_ms,
                frozen_ms,
            })
        })
        .collect()
}

fn pushed(body: &str, line: &str) -> String {
    let mut lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    lines.push(line);
    let from = lines.len().saturating_sub(RING);
    let mut out = lines[from..].join("\n");
    out.push('\n');
    out
}

pub(crate) fn ring_path(dir: &Path, wrapper: u32) -> PathBuf {
    turn_file(dir, wrapper, ".pace")
}

pub(crate) fn last_mark(dir: &Path, wrapper: u32) -> Option<u128> {
    fs::read_to_string(turn_file(dir, wrapper, ".last"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn frozen_path(dir: &Path, wrapper: u32) -> PathBuf {
    turn_file(dir, wrapper, ".frozen")
}

pub(crate) fn read_frozen(dir: &Path, wrapper: u32) -> Option<u64> {
    fs::read_to_string(frozen_path(dir, wrapper))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn unfrozen_gap(
    gap_ms: Option<f64>,
    frozen_now: Option<u64>,
    frozen_then: Option<u64>,
) -> Option<f64> {
    let between = match (frozen_now, frozen_then) {
        (Some(now), Some(then)) => now.saturating_sub(then) as f64,
        _ => 0.0,
    };
    gap_ms.map(|gap| (gap - between).max(0.0))
}

pub(crate) fn clamp(report: Report, unfrozen_ms: Option<f64>) -> Report {
    let cap = |value: f64| unfrozen_ms.map_or(value, |most| value.min(most));
    let work_ms = report.work_ms.map(cap);
    let longest_ms = report.longest_ms.map(cap).map(|longest| match work_ms {
        Some(work) => longest.min(work),
        None => longest,
    });
    Report {
        work_ms,
        longest_ms,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Noted {
    pub(crate) last: Option<u128>,
    pub(crate) frozen: Option<u64>,
}

pub(crate) fn note(
    dir: &Path,
    wrapper: u32,
    report: Report,
    now: u128,
    began: Option<u128>,
) -> Noted {
    let last = last_mark(dir, wrapper);
    let path = ring_path(dir, wrapper);
    let body = fs::read_to_string(&path).unwrap_or_default();
    let frozen = read_frozen(dir, wrapper);
    let since = |from: u128| (from <= now).then(|| (now - from) as f64 / 1e6);
    let raw_gap = last.and_then(since);
    let gap_ms = unfrozen_gap(
        raw_gap,
        frozen,
        parse_ring(&body).last().and_then(|e| e.frozen_ms),
    );
    let bound = match last {
        Some(_) => gap_ms,
        None => unfrozen_gap(began.and_then(since), frozen, Some(0)),
    };
    let report = clamp(report, bound);
    let entry = Entry {
        at: now,
        work_ms: report.work_ms,
        longest_ms: report.longest_ms,
        gap_ms,
        frozen_ms: frozen,
    };
    let _ = write_atomic(&path, &pushed(&body, &entry_line(&entry)));
    Noted { last, frozen }
}

pub(crate) fn returned(dir: &Path, wrapper: u32, noted: Option<u64>) {
    let Some(frozen) = read_frozen(dir, wrapper).filter(|now| Some(*now) != noted) else {
        return;
    };
    let path = ring_path(dir, wrapper);
    let body = fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let Some(mut entry) = lines.last().and_then(|l| parse_ring(l).pop()) else {
        return;
    };
    entry.frozen_ms = Some(frozen);
    if let Some(slot) = lines.last_mut() {
        *slot = entry_line(&entry);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    let _ = write_atomic(&path, &out);
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Frozen {
    total_ms: u64,
    since: Option<Instant>,
}

impl Frozen {
    pub(crate) fn act(&mut self, act: &Act, at: Instant) -> Option<u64> {
        match act {
            Act::Park => {
                self.since.get_or_insert(at);
                None
            }
            Act::Resume => {
                let since = self.since.take()?;
                let ms = at.saturating_duration_since(since).as_millis();
                self.total_ms = self
                    .total_ms
                    .saturating_add(u64::try_from(ms).unwrap_or(u64::MAX));
                Some(self.total_ms)
            }
            _ => None,
        }
    }

    pub(crate) fn waited(&mut self, wait: Duration) -> u64 {
        self.total_ms = self
            .total_ms
            .saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX));
        self.total_ms
    }

    pub(crate) fn ms(&self, at: Instant) -> u64 {
        let ongoing = self.since.map_or(0, |since| {
            u64::try_from(at.saturating_duration_since(since).as_millis()).unwrap_or(u64::MAX)
        });
        self.total_ms.saturating_add(ongoing)
    }
}

pub(crate) fn ring(path: &Path) -> Vec<Entry> {
    parse_ring(&fs::read_to_string(path).unwrap_or_default())
}

fn gaps(entries: &[Entry]) -> Vec<Option<f64>> {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            e.gap_ms.or_else(|| {
                i.checked_sub(1)
                    .map(|j| entries[j].at)
                    .filter(|prev| *prev <= e.at)
                    .map(|prev| (e.at - prev) as f64 / 1e6)
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Summary {
    pub(crate) turn_gap_ms: Option<f64>,
    pub(crate) work_ms: Option<f64>,
    pub(crate) longest_ms: Option<f64>,
}

impl Summary {
    pub(crate) fn reported(&self) -> bool {
        self.work_ms.is_some() || self.longest_ms.is_some()
    }

    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"turn_gap_ms\":{},\"work_ms\":{},\"longest_ms\":{}}}",
            num(self.turn_gap_ms),
            num(self.work_ms),
            num(self.longest_ms)
        )
    }

    pub(crate) fn note(&self) -> Option<String> {
        if !self.reported() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some(longest) = self.longest_ms {
            parts.push(format!("longest step {}", short_ms(longest)));
        }
        if let Some(gap) = self.turn_gap_ms {
            parts.push(format!("turn gap {}", short_ms(gap)));
        }
        Some(format!("pace: {}", parts.join(", ")))
    }
}

fn num(value: Option<f64>) -> String {
    value.map_or("null".to_string(), |v| format!("{:.1}", v))
}

fn short_ms(ms: f64) -> String {
    if ms >= 10_000.0 {
        format!("{:.0} s", ms / 1000.0)
    } else if ms >= 100.0 {
        format!("{:.0} ms", ms)
    } else {
        format!("{:.1} ms", ms)
    }
}

pub(crate) fn summary(entries: &[Entry], now: u128, open_from: Option<u128>) -> Option<Summary> {
    let last = entries.last()?;
    let from = now.saturating_sub(STATUS_WINDOW_NS);
    let all_gaps = gaps(entries);
    let mut turn_gap: Option<f64> = None;
    let mut work = Vec::new();
    let mut longest: Option<f64> = None;
    for (entry, gap) in entries.iter().zip(all_gaps) {
        if entry.at < from {
            continue;
        }
        if let Some(gap) = gap {
            turn_gap = Some(turn_gap.map_or(gap, |was: f64| was.max(gap)));
        }
        if let Some(w) = entry.work_ms {
            work.push(w);
        }
        if let Some(l) = entry.longest_ms {
            longest = Some(longest.map_or(l, |was: f64| was.max(l)));
        }
    }
    if let Some(open) = open_from {
        let since = open.max(last.at);
        if since <= now {
            let gap = (now - since) as f64 / 1e6;
            turn_gap = Some(turn_gap.map_or(gap, |was| was.max(gap)));
        }
    }
    Some(Summary {
        turn_gap_ms: turn_gap,
        work_ms: (!work.is_empty()).then(|| work.iter().sum::<f64>() / work.len() as f64),
        longest_ms: longest,
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Fold {
    after: Option<u128>,
    turns: u64,
    reported: u64,
    gaps: Vec<f32>,
    longest: Vec<f32>,
}

impl Fold {
    pub(crate) fn fold(&mut self, entries: &[Entry], started: u128) {
        let all_gaps = gaps(entries);
        for (entry, gap) in entries.iter().zip(all_gaps) {
            if self.after.is_some_and(|after| entry.at <= after) {
                continue;
            }
            let gap = gap.or_else(|| {
                (self.after.is_none() && started <= entry.at).then(|| {
                    let since = (entry.at - started) as f64 / 1e6;
                    (since - entry.frozen_ms.unwrap_or(0) as f64).max(0.0)
                })
            });
            self.turns += 1;
            if let Some(gap) = gap {
                self.gaps.push(gap as f32);
            }
            if entry.reported() {
                self.reported += 1;
            }
            if let Some(longest) = entry.longest_ms {
                self.longest.push(longest as f32);
            }
            self.after = Some(entry.at);
        }
    }

    pub(crate) fn line(&self, end_ms: u128, who: &str, class: u8) -> String {
        format!(
            "{end_ms}\t{who}\t{class}\t{}\t{}\t{}\t{}\n",
            self.turns,
            self.reported,
            spread(&self.gaps),
            spread(&self.longest)
        )
    }
}

pub(crate) fn pick<T: Copy>(sorted: &[T], q: f64) -> T {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn sorted(values: &[f32]) -> Vec<f32> {
    let mut values = values.to_vec();
    values.sort_by(f32::total_cmp);
    values
}

fn spread(values: &[f32]) -> String {
    if values.is_empty() {
        return "-\t-\t-".to_string();
    }
    let sorted = sorted(values);
    format!(
        "{:.1}\t{:.1}\t{:.1}",
        pick(&sorted, 0.5),
        pick(&sorted, 0.9),
        pick(&sorted, 1.0)
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Session {
    pub(crate) who: String,
    pub(crate) jobs: u64,
    pub(crate) turns: u64,
    pub(crate) reported: u64,
    pub(crate) gaps: Option<[f32; 3]>,
    pub(crate) longest: Option<[f32; 3]>,
}

#[derive(Default)]
struct Gather {
    jobs: u64,
    turns: u64,
    reported: u64,
    gaps: [Vec<f32>; 3],
    longest: [Vec<f32>; 3],
}

fn triple(fields: &[&str]) -> Option<[f32; 3]> {
    let mut out = [0.0f32; 3];
    for (slot, field) in out.iter_mut().zip(fields) {
        *slot = field.parse::<f32>().ok().filter(|v| v.is_finite())?;
    }
    (fields.len() == 3).then_some(out)
}

fn combine(parts: &[Vec<f32>; 3]) -> Option<[f32; 3]> {
    if parts[0].is_empty() {
        return None;
    }
    Some([
        pick(&sorted(&parts[0]), 0.5),
        pick(&sorted(&parts[1]), 0.9),
        pick(&sorted(&parts[2]), 1.0),
    ])
}

pub(crate) fn sessions(body: &str, since_ms: u128) -> Vec<Session> {
    let mut by: BTreeMap<String, Gather> = BTreeMap::new();
    for line in body.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 11 {
            continue;
        }
        let (Ok(end), Ok(turns), Ok(reported)) = (
            f[0].parse::<u128>(),
            f[3].parse::<u64>(),
            f[4].parse::<u64>(),
        ) else {
            continue;
        };
        if end < since_ms {
            continue;
        }
        let gather = by.entry(f[1].to_string()).or_default();
        gather.jobs += 1;
        gather.turns += turns;
        gather.reported += reported;
        if let Some(gaps) = triple(&f[5..8]) {
            for (all, v) in gather.gaps.iter_mut().zip(gaps) {
                all.push(v);
            }
        }
        if let Some(longest) = triple(&f[8..11]) {
            for (all, v) in gather.longest.iter_mut().zip(longest) {
                all.push(v);
            }
        }
    }
    by.into_iter()
        .map(|(who, g)| Session {
            who,
            jobs: g.jobs,
            turns: g.turns,
            reported: g.reported,
            gaps: combine(&g.gaps),
            longest: combine(&g.longest),
        })
        .collect()
}

impl Session {
    pub(crate) fn reported_pct(&self) -> u64 {
        (self.reported * 100).checked_div(self.turns).unwrap_or(0)
    }
}

fn triple_json(values: Option<[f32; 3]>) -> String {
    values.map_or("null".to_string(), |[p50, p90, max]| {
        format!("{{\"p50\":{p50:.1},\"p90\":{p90:.1},\"max\":{max:.1}}}")
    })
}

fn triple_text(values: Option<[f32; 3]>) -> String {
    values.map_or("-".to_string(), |[p50, p90, max]| {
        format!(
            "{} / {} / {}",
            short_ms(p50.into()),
            short_ms(p90.into()),
            short_ms(max.into())
        )
    })
}

pub(crate) fn render(sessions: &[Session], days: u128, json: bool) -> String {
    if json {
        let rows: Vec<String> = sessions
            .iter()
            .map(|s| {
                format!(
                    "{{\"as\":{},\"jobs\":{},\"turns\":{},\"reported_pct\":{},\"turn_gap_ms\":{},\"longest_ms\":{}}}",
                    crate::report::quote(&s.who),
                    s.jobs,
                    s.turns,
                    s.reported_pct(),
                    triple_json(s.gaps),
                    triple_json(s.longest)
                )
            })
            .collect();
        return format!(
            "{{\"v\":1,\"days\":{days},\"sessions\":[{}]}}\n",
            rows.join(",")
        );
    }
    if sessions.is_empty() {
        return format!("no jobs logged in the last {days} days\n");
    }
    let mut out = format!(
        "{:<14}  {:>5}  {:>7}  {:>8}  {:>26}  {:>26}\n",
        "session",
        "jobs",
        "turns",
        "reported",
        "turn gap p50 / p90 / max",
        "longest p50 / p90 / max"
    );
    for s in sessions {
        out.push_str(&format!(
            "{:<14}  {:>5}  {:>7}  {:>7}%  {:>26}  {:>26}\n",
            s.who,
            s.jobs,
            s.turns,
            s.reported_pct(),
            triple_text(s.gaps),
            triple_text(s.longest)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        Entry, Fold, Frozen, RING, Report, STATUS_WINDOW_NS, clamp, entry_line, frozen_path, note,
        parse_ring, pushed, render, report, returned, ring, ring_path, sessions, summary,
        unfrozen_gap,
    };
    use crate::hour::append;
    use crate::wrapper::Act;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-pace-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("turns")).unwrap();
        path
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const MS: u128 = 1_000_000;

    fn at(ms: u128, work: Option<f64>, longest: Option<f64>, gap: Option<f64>) -> Entry {
        Entry {
            at: ms * MS,
            work_ms: work,
            longest_ms: longest,
            gap_ms: gap,
            frozen_ms: None,
        }
    }

    #[test]
    fn the_flags_parse_and_anything_else_is_ignored() {
        assert_eq!(report(&[]), Report::default());
        assert_eq!(
            report(&args(&["--work-ms", "40", "--longest-ms", "3.5"])),
            Report {
                work_ms: Some(40.0),
                longest_ms: Some(3.5)
            }
        );
        assert_eq!(
            report(&args(&["--longest-ms=80", "--work-ms=12.25"])),
            Report {
                work_ms: Some(12.25),
                longest_ms: Some(80.0)
            }
        );
        assert_eq!(
            report(&args(&["--work-ms", "--longest-ms", "7"])),
            Report {
                work_ms: None,
                longest_ms: Some(7.0)
            }
        );
        for bad in [
            &["--work-ms", "abc"][..],
            &["--work-ms", "-3"],
            &["--work-ms", "NaN"],
            &["--work-ms", "inf"],
            &["--work-ms"],
            &["--work-ms="],
            &["--frobnicate", "9", "x"],
            &["--WORK-MS", "9"],
        ] {
            assert_eq!(report(&args(bad)), Report::default(), "{bad:?}");
        }
        assert_eq!(
            report(&args(&["junk", "--work-ms", "5", "--work-ms", "6"])),
            Report {
                work_ms: Some(6.0),
                longest_ms: None
            }
        );
    }

    #[test]
    fn an_entry_round_trips_and_bad_lines_are_skipped() {
        let e = at(1234, Some(40.0), Some(3.5), Some(51.25));
        assert_eq!(entry_line(&e), "1234000000\t40\t3.5\t51.25\t-");
        let thawed = Entry {
            frozen_ms: Some(4_200),
            ..e
        };
        assert_eq!(entry_line(&thawed), "1234000000\t40\t3.5\t51.25\t4200");
        assert_eq!(parse_ring(&entry_line(&thawed)), vec![thawed]);
        assert_eq!(
            parse_ring("1234000000\t40\t3.5\t51.25\n"),
            vec![e],
            "a line from before the column"
        );
        let plain = at(5, None, None, None);
        assert_eq!(entry_line(&plain), "5000000\t-\t-\t-\t-");
        let body = format!(
            "{}\nnot a line\n{}\n7\n",
            entry_line(&e),
            entry_line(&plain)
        );
        let parsed = parse_ring(&body);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], e);
        assert_eq!(parsed[1], plain);
        assert_eq!(parsed[2], Entry { at: 7, ..plain });
    }

    #[test]
    fn the_ring_appends_and_keeps_the_last_64() {
        let dir = test_dir("ring");
        let path = ring_path(&dir, 42);
        assert!(ring(&path).is_empty());
        note(
            &dir,
            42,
            Report {
                work_ms: Some(40.0),
                longest_ms: Some(3.5),
            },
            1_000 * MS,
            None,
        );
        let first = ring(&path);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].gap_ms, None);
        assert_eq!(first[0].work_ms, Some(40.0));
        fs::write(dir.join("turns/42.last"), (1_010 * MS).to_string()).unwrap();
        note(&dir, 42, Report::default(), 1_060 * MS, None);
        let second = ring(&path);
        assert_eq!(second.len(), 2);
        assert_eq!(second[1].gap_ms, Some(50.0));
        assert_eq!(second[1].longest_ms, None);
        for i in 0..100u128 {
            note(&dir, 42, Report::default(), (2_000 + i) * MS, None);
        }
        let full = ring(&path);
        assert_eq!(full.len(), RING);
        assert_eq!(full.last().unwrap().at, 2_099 * MS);
        assert_eq!(full[0].at, (2_100 - RING as u128) * MS);
        assert_eq!(fs::read_dir(dir.join("turns")).unwrap().count(), 2);
        let mut body = String::new();
        for i in 0..70 {
            body = pushed(&body, &i.to_string());
        }
        assert_eq!(body.lines().count(), RING);
        assert_eq!(body.lines().next(), Some("6"));
    }

    #[test]
    fn the_status_window_is_the_last_10_s_with_the_open_gap() {
        assert_eq!(summary(&[], 0, None), None);
        let now = 100_000 * MS;
        let old = now / MS - 10_001;
        let entries = vec![
            at(old, Some(900.0), Some(900.0), Some(5_000.0)),
            at(now / MS - 3_000, Some(40.0), Some(3.5), Some(60.0)),
            at(now / MS - 2_930, None, None, None),
            at(now / MS - 2_860, Some(20.0), Some(4.5), Some(52.0)),
        ];
        let got = summary(&entries, now, None).unwrap();
        assert_eq!(got.turn_gap_ms, Some(70.0));
        assert_eq!(got.work_ms, Some(30.0));
        assert_eq!(got.longest_ms, Some(4.5));
        assert!(got.reported());
        assert_eq!(
            got.json(),
            "{\"turn_gap_ms\":70.0,\"work_ms\":30.0,\"longest_ms\":4.5}"
        );
        let open = summary(&entries, now, Some(now - 2_800 * MS)).unwrap();
        assert_eq!(open.turn_gap_ms, Some(2_800.0));
        let quiet = summary(&[at(old, None, None, None)], now, None).unwrap();
        assert_eq!(quiet.turn_gap_ms, None);
        assert_eq!(quiet.work_ms, None);
        assert_eq!(quiet.longest_ms, None);
        assert!(!quiet.reported());
        assert_eq!(quiet.note(), None);
        assert_eq!(
            quiet.json(),
            "{\"turn_gap_ms\":null,\"work_ms\":null,\"longest_ms\":null}"
        );
        let stalled = summary(&[at(old, None, None, None)], now, Some(0)).unwrap();
        assert_eq!(stalled.turn_gap_ms, Some(10_001.0));
        assert_eq!(
            got.note().as_deref(),
            Some("pace: longest step 4.5 ms, turn gap 70.0 ms")
        );
        assert_eq!(STATUS_WINDOW_NS, 10_000 * MS);
    }

    #[test]
    fn a_fold_counts_each_turn_once_and_judges_from_reports() {
        let mut fold = Fold::default();
        fold.fold(&[], 0);
        let paced = vec![
            at(1_050, Some(40.0), Some(3.5), None),
            at(1_100, Some(40.0), Some(3.5), Some(50.0)),
        ];
        fold.fold(&paced, 1_000 * MS);
        assert_eq!(fold.turns, 2);
        assert_eq!(fold.reported, 2);
        assert_eq!(fold.gaps, vec![50.0, 50.0]);
        fold.fold(&paced, 1_000 * MS);
        assert_eq!(fold.turns, 2, "a turn was folded twice");
        let mut more = paced.clone();
        more.push(at(1_200, Some(80.0), Some(80.0), Some(100.0)));
        fold.fold(&more, 1_000 * MS);
        more.push(at(1_250, None, None, Some(50.0)));
        fold.fold(&more, 1_000 * MS);
        assert_eq!(fold.turns, 4);
        assert_eq!(fold.reported, 3);
        more.push(at(1_400, Some(150.0), Some(90.0), Some(150.0)));
        fold.fold(&more, 1_000 * MS);
        more.push(at(1_450, Some(3.0), Some(3.0), Some(50.0)));
        fold.fold(&more, 1_000 * MS);
        assert_eq!(
            fold.line(9, "studio", 1),
            "9\tstudio\t1\t6\t5\t50.0\t150.0\t150.0\t3.5\t90.0\t90.0\n"
        );
        assert_eq!(
            Fold::default().line(9, "blender", 2),
            "9\tblender\t2\t0\t0\t-\t-\t-\t-\t-\t-\n"
        );
    }

    #[test]
    fn the_pace_log_sums_per_session_and_stats_render_it() {
        let dir = test_dir("log");
        let path = dir.join("pace.tsv");
        append(
            &path,
            "1000\tstudio\t1\t100\t100\t50.0\t70.0\t90.0\t3.5\t5.0\t6.0\n",
        );
        append(
            &path,
            "2000\tstudio\t1\t50\t0\t60.0\t80.0\t400.0\t-\t-\t-\n",
        );
        append(&path, "3000\tblender\t2\t0\t0\t-\t-\t-\t-\t-\t-\n");
        append(&path, "10\told\t2\t1\t1\t1\t1\t1\t1\t1\t1\n");
        append(&path, "broken line\n");
        let body = fs::read_to_string(&path).unwrap();
        let rows = sessions(&body, 500);
        assert_eq!(rows.len(), 2);
        let blender = &rows[0];
        assert_eq!(blender.who, "blender");
        assert_eq!(blender.jobs, 1);
        assert_eq!(blender.turns, 0);
        assert_eq!(blender.reported_pct(), 0);
        assert_eq!(blender.gaps, None);
        let studio = &rows[1];
        assert_eq!(studio.jobs, 2);
        assert_eq!(studio.turns, 150);
        assert_eq!(studio.reported_pct(), 66);
        assert_eq!(studio.gaps, Some([60.0, 80.0, 400.0]));
        assert_eq!(studio.longest, Some([3.5, 5.0, 6.0]));
        let json = render(&rows, 7, true);
        assert_eq!(
            json,
            "{\"v\":1,\"days\":7,\"sessions\":[\
             {\"as\":\"blender\",\"jobs\":1,\"turns\":0,\"reported_pct\":0,\"turn_gap_ms\":null,\"longest_ms\":null},\
             {\"as\":\"studio\",\"jobs\":2,\"turns\":150,\"reported_pct\":66,\
             \"turn_gap_ms\":{\"p50\":60.0,\"p90\":80.0,\"max\":400.0},\
             \"longest_ms\":{\"p50\":3.5,\"p90\":5.0,\"max\":6.0}}]}\n"
        );
        let text = render(&rows, 7, false);
        assert!(text.contains("studio"), "{text}");
        assert!(text.contains("60.0 ms / 80.0 ms / 400 ms"), "{text}");
        assert!(text.contains("66%"), "{text}");
        assert_eq!(render(&[], 3, false), "no jobs logged in the last 3 days\n");
    }

    fn both(work: f64, longest: f64) -> Report {
        Report {
            work_ms: Some(work),
            longest_ms: Some(longest),
        }
    }

    #[test]
    fn a_report_is_clamped_to_the_unfrozen_time_and_the_longest_to_the_work() {
        assert_eq!(clamp(both(500.0, 500.0), Some(120.0)), both(120.0, 120.0));
        assert_eq!(clamp(both(40.0, 90.0), Some(500.0)), both(40.0, 40.0));
        assert_eq!(clamp(both(40.0, 3.5), Some(500.0)), both(40.0, 3.5));
        assert_eq!(clamp(both(40.0, 3.5), None), both(40.0, 3.5));
        assert_eq!(clamp(both(500.0, 900.0), None), both(500.0, 500.0));
        assert_eq!(clamp(both(40.0, 3.5), Some(0.0)), both(0.0, 0.0));
        assert_eq!(clamp(Report::default(), Some(10.0)), Report::default());
        let work_only = Report {
            work_ms: Some(80.0),
            longest_ms: None,
        };
        assert_eq!(clamp(work_only, Some(30.0)).work_ms, Some(30.0));
        assert_eq!(clamp(work_only, Some(30.0)).longest_ms, None);
        let longest_only = Report {
            work_ms: None,
            longest_ms: Some(80.0),
        };
        assert_eq!(clamp(longest_only, Some(30.0)).longest_ms, Some(30.0));
        assert_eq!(clamp(longest_only, Some(300.0)).longest_ms, Some(80.0));
    }

    #[test]
    fn the_unfrozen_gap_leaves_out_what_was_frozen_between_two_turns() {
        assert_eq!(
            unfrozen_gap(Some(1_000.0), Some(900), Some(100)),
            Some(200.0)
        );
        assert_eq!(
            unfrozen_gap(Some(1_000.0), Some(5_000), Some(100)),
            Some(0.0)
        );
        assert_eq!(
            unfrozen_gap(Some(1_000.0), Some(100), Some(100)),
            Some(1_000.0)
        );
        assert_eq!(unfrozen_gap(Some(1_000.0), None, Some(100)), Some(1_000.0));
        assert_eq!(unfrozen_gap(Some(1_000.0), Some(100), None), Some(1_000.0));
        assert_eq!(unfrozen_gap(None, Some(100), Some(0)), None);
    }

    #[test]
    fn a_turn_clamps_by_the_frozen_file_and_keeps_only_the_clamped_numbers() {
        let dir = test_dir("clamp");
        let path = ring_path(&dir, 42);
        fs::write(frozen_path(&dir, 42), "0").unwrap();
        let first = note(&dir, 42, both(500.0, 500.0), 1_000 * MS, None);
        assert_eq!(first.last, None);
        assert_eq!(first.frozen, Some(0));
        assert_eq!(
            ring(&path)[0].work_ms,
            Some(500.0),
            "no gap, nothing to clamp by"
        );
        assert_eq!(ring(&path)[0].frozen_ms, Some(0));
        fs::write(dir.join("turns/42.last"), (1_010 * MS).to_string()).unwrap();
        fs::write(frozen_path(&dir, 42), "800").unwrap();
        note(&dir, 42, both(500.0, 500.0), 2_000 * MS, None);
        let second = ring(&path)[1];
        assert_eq!(second.gap_ms, Some(190.0), "990 ms less 800 frozen");
        assert_eq!(second.work_ms, Some(190.0));
        assert_eq!(second.longest_ms, Some(190.0));
        assert_eq!(second.frozen_ms, Some(800));
        fs::write(dir.join("turns/42.last"), (2_010 * MS).to_string()).unwrap();
        note(&dir, 42, both(30.0, 3.0), 2_060 * MS, None);
        let third = ring(&path)[2];
        assert_eq!(third.gap_ms, Some(50.0));
        assert_eq!(third.work_ms, Some(30.0));
        assert_eq!(third.longest_ms, Some(3.0));
    }

    #[test]
    fn the_first_turn_is_clamped_by_the_time_since_the_job_started_less_its_frozen_time() {
        let dir = test_dir("first");
        let path = ring_path(&dir, 42);
        fs::write(frozen_path(&dir, 42), "4000").unwrap();
        note(
            &dir,
            42,
            both(4_413.0, 4_413.0),
            5_000 * MS,
            Some(1_000 * MS),
        );
        let first = ring(&path)[0];
        assert_eq!(
            first.work_ms,
            Some(0.0),
            "4 s since the start, all of it frozen"
        );
        assert_eq!(first.gap_ms, None, "the first turn has no gap");
        fs::write(frozen_path(&dir, 42), "1000").unwrap();
        fs::remove_file(&path).unwrap();
        note(
            &dir,
            42,
            both(4_413.0, 4_413.0),
            5_000 * MS,
            Some(1_000 * MS),
        );
        assert_eq!(ring(&path)[0].work_ms, Some(3_000.0));
        fs::remove_file(&path).unwrap();
        note(&dir, 42, both(4_413.0, 4_413.0), 5_000 * MS, None);
        assert_eq!(ring(&path)[0].work_ms, Some(4_413.0), "no start, no bound");
    }

    #[test]
    fn a_turn_with_no_frozen_file_or_an_old_ring_line_clamps_by_the_plain_gap() {
        let dir = test_dir("plain");
        let path = ring_path(&dir, 42);
        note(&dir, 42, Report::default(), 1_000 * MS, None);
        fs::write(dir.join("turns/42.last"), (1_010 * MS).to_string()).unwrap();
        note(&dir, 42, both(500.0, 500.0), 1_060 * MS, None);
        let entries = ring(&path);
        assert_eq!(entries[1].gap_ms, Some(50.0));
        assert_eq!(entries[1].work_ms, Some(50.0));
        assert_eq!(entries[1].frozen_ms, None);
        fs::write(&path, "1000000000\t-\t-\t-\n1060000000\t5\t5\t50\n").unwrap();
        fs::write(frozen_path(&dir, 42), "9000").unwrap();
        fs::write(dir.join("turns/42.last"), (1_070 * MS).to_string()).unwrap();
        note(&dir, 42, both(500.0, 500.0), 1_120 * MS, None);
        let entries = ring(&path);
        assert_eq!(
            entries[2].gap_ms,
            Some(50.0),
            "an old line has no frozen value to subtract"
        );
        assert_eq!(entries[2].work_ms, Some(50.0));
        assert_eq!(entries[2].frozen_ms, Some(9_000));
    }

    #[test]
    fn freezing_inside_a_turn_does_not_count_against_the_next_gap() {
        let dir = test_dir("inside");
        let path = ring_path(&dir, 42);
        fs::write(frozen_path(&dir, 42), "100").unwrap();
        let noted = note(&dir, 42, Report::default(), 1_000 * MS, None);
        fs::write(frozen_path(&dir, 42), "10100").unwrap();
        returned(&dir, 42, noted.frozen);
        assert_eq!(ring(&path)[0].frozen_ms, Some(10_100));
        returned(&dir, 42, Some(10_100));
        fs::write(dir.join("turns/42.last"), (11_000 * MS).to_string()).unwrap();
        note(&dir, 42, both(500.0, 500.0), 11_060 * MS, None);
        let entries = ring(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].gap_ms, Some(60.0));
        assert_eq!(entries[1].work_ms, Some(60.0));
    }

    #[test]
    fn a_fold_leaves_the_frozen_time_out_of_the_first_gap() {
        let mut fold = Fold::default();
        let first = Entry {
            frozen_ms: Some(300),
            ..at(1_500, None, None, None)
        };
        fold.fold(&[first], 1_000 * MS);
        assert_eq!(fold.gaps, vec![200.0]);
    }

    fn after(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn frozen_time_adds_up_across_parks_and_resumes() {
        let t = Instant::now();
        let mut frozen = Frozen::default();
        assert_eq!(frozen.act(&Act::Resume, after(t, 10)), None);
        assert_eq!(frozen.act(&Act::Park, after(t, 100)), None);
        assert_eq!(frozen.act(&Act::Resume, after(t, 400)), Some(300));
        assert_eq!(frozen.act(&Act::HandOver, after(t, 500)), None);
        assert_eq!(frozen.act(&Act::Park, after(t, 1_000)), None);
        assert_eq!(frozen.act(&Act::Park, after(t, 1_100)), None, "one freeze");
        assert_eq!(frozen.act(&Act::Resume, after(t, 1_250)), Some(550));
        assert_eq!(frozen.act(&Act::Resume, after(t, 1_300)), None);
        assert_eq!(frozen.waited(Duration::from_millis(2_000)), 2_550);
        assert_eq!(frozen.act(&Act::Park, after(t, 3_000)), None);
        assert_eq!(
            frozen.ms(after(t, 3_400)),
            2_950,
            "a freeze in progress counts"
        );
    }

    #[test]
    fn a_desk_hold_a_play_hold_and_a_take_back_count_from_their_park_to_the_resume() {
        let t = Instant::now();
        let mut frozen = Frozen::default();
        let mut at = 0;
        let mut run = |frozen: &mut Frozen, acts: &[(u64, Act)]| {
            let mut out = None;
            for (ms, act) in acts {
                at += ms;
                out = frozen.act(act, after(t, at)).or(out);
            }
            out
        };
        let desk = run(
            &mut frozen,
            &[(0, Act::Park), (0, Act::HandOver), (4_000, Act::Resume)],
        );
        assert_eq!(desk, Some(4_000));
        let play = run(
            &mut frozen,
            &[(100, Act::Park), (0, Act::Lend), (2_500, Act::Resume)],
        );
        assert_eq!(play, Some(6_500));
        let take_back = run(
            &mut frozen,
            &[
                (100, Act::Lend),
                (900, Act::Park),
                (0, Act::TakeBack),
                (700, Act::Resume),
            ],
        );
        assert_eq!(take_back, Some(7_200));
        let share = run(
            &mut frozen,
            &[
                (50, Act::Park),
                (150, Act::Resume),
                (50, Act::Park),
                (150, Act::Resume),
            ],
        );
        assert_eq!(share, Some(7_500));
    }
}
