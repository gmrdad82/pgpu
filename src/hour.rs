use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::paths::{write_atomic, write_atomic_bytes};

const HOUR_MS: u128 = 3_600_000;
const TAIL_BYTES: u64 = 512 * 1024;
const TRIM_AT: u64 = 32 * 1024 * 1024;
const KEEP_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_MS: u128 = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub struct Hour {
    pub held_pct: u128,
    pub jobs: usize,
    pub holds: usize,
    pub gave_way: usize,
    pub wait_ms: [u128; 3],
}

impl Hour {
    fn json(&self) -> String {
        format!(
            "{{\"held_pct\":{},\"jobs\":{},\"holds\":{},\"gave_way\":{},\"wait_ms\":{{\"p50\":{},\"p90\":{},\"max\":{}}}}}",
            self.held_pct,
            self.jobs,
            self.holds,
            self.gave_way,
            self.wait_ms[0],
            self.wait_ms[1],
            self.wait_ms[2]
        )
    }
}

pub fn line(
    from: u128,
    to: u128,
    who: &str,
    class: u8,
    waited: u128,
    pid: u32,
    end: &str,
) -> String {
    format!("{from}\t{to}\t{who}\t{class}\t{waited}\t{pid}\t{end}\n")
}

pub fn summarize(lines: &str, running: &[(u128, u32)], now: u128) -> Option<Hour> {
    let since = now.saturating_sub(HOUR_MS);
    let mut intervals = Vec::new();
    let mut pids = BTreeSet::new();
    let mut waits = Vec::new();
    let mut holds = 0;
    let mut gave_way = 0;
    for line in lines.lines() {
        let mut fields = line.split('\t');
        let (Some(from), Some(to), Some(_), Some(_), Some(waited)) = (
            fields.next().and_then(|v| v.parse::<u128>().ok()),
            fields.next().and_then(|v| v.parse::<u128>().ok()),
            fields.next(),
            fields.next(),
            fields.next().and_then(|v| v.parse::<u128>().ok()),
        ) else {
            continue;
        };
        if from >= to || from >= now || to <= since {
            continue;
        }
        let end = to.min(now);
        if from >= end {
            continue;
        }
        intervals.push((from.max(since), end));
        if let Some(pid) = fields.next().and_then(|v| v.parse::<u32>().ok()) {
            pids.insert(pid);
        }
        if to <= now {
            holds += 1;
            waits.push(waited);
            if matches!(
                fields.next(),
                Some("turn" | "desk" | "lend" | "play" | "reclaim")
            ) {
                gave_way += 1;
            }
        }
    }
    for &(from, pid) in running.iter().filter(|(from, _)| *from < now) {
        let start = from.max(since);
        if start < now {
            intervals.push((start, now));
            pids.insert(pid);
        }
    }
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_unstable();
    let mut covered = 0;
    let (mut start, mut end) = intervals[0];
    for &(next_start, next_end) in &intervals[1..] {
        if next_start > end {
            covered += end - start;
            start = next_start;
            end = next_end;
        } else {
            end = end.max(next_end);
        }
    }
    covered += end - start;
    waits.sort_unstable();
    let pick = |q: f64| {
        waits
            .get(((waits.len().saturating_sub(1)) as f64 * q).round() as usize)
            .copied()
            .unwrap_or(0)
    };
    Some(Hour {
        held_pct: covered * 100 / HOUR_MS,
        jobs: pids.len(),
        holds,
        gave_way,
        wait_ms: [pick(0.5), pick(0.9), pick(1.0)],
    })
}

fn tail(path: &Path, bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(bytes);
    let boundary = if start == 0 {
        true
    } else {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0];
        file.read_exact(&mut previous)?;
        previous[0] == b'\n'
    };
    file.seek(SeekFrom::Start(start))?;
    let mut body = Vec::new();
    file.take(len - start).read_to_end(&mut body)?;
    if !boundary {
        let after = body
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(body.len(), |at| at + 1);
        body.drain(..after);
    }
    Ok(body)
}

pub fn read_tail(path: &Path) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&tail(path, TAIL_BYTES)?).into_owned())
}

pub fn trim(path: &Path) -> io::Result<()> {
    if fs::metadata(path)?.len() <= TRIM_AT {
        return Ok(());
    }
    write_atomic_bytes(path, &tail(path, KEEP_BYTES)?)
}

pub fn append(path: &Path, line: &str) {
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
        let _ = trim(path);
    }
}

pub fn cached_json(dir: &Path, log: &Path, running: &[(u128, u32)], now: u128) -> String {
    let cache = dir.join("hour");
    if let Some(json) = crate::cache::read(&cache, now, CACHE_MS)
        .as_deref()
        .and_then(|rest| rest.lines().next())
    {
        return json.to_string();
    }
    let json = summarize(&read_tail(log).unwrap_or_default(), running, now)
        .map_or("null".to_string(), |hour| hour.json());
    let _ = write_atomic(&cache, &format!("{now}\n{json}\n"));
    json
}

#[cfg(test)]
mod tests {
    use super::{Hour, cached_json, line, read_tail, summarize, trim};
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-hour-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn overlapping_old_and_running_holds() {
        let now = 4_000_000;
        let lines = "100000\t500000\ta\t1\t100\t10\tdesk\n400000\t700000\tb\t1\t200\t11\tturn\n600000\t1000000\tc\t2\t300\n2000000\t2100000\td\t1\t400\t10\tsignal\n";
        assert_eq!(
            summarize(lines, &[(2_000_000, 12)], now),
            Some(Hour {
                held_pct: 72,
                jobs: 3,
                holds: 4,
                gave_way: 2,
                wait_ms: [300, 400, 400],
            })
        );
    }

    #[test]
    fn empty_window_and_running_only() {
        assert_eq!(summarize("1\t2\ta\t1\t3\n", &[], 4_000_000), None);
        assert_eq!(
            summarize("", &[(3_000_000, 42)], 4_000_000),
            Some(Hour {
                held_pct: 27,
                jobs: 1,
                holds: 0,
                gave_way: 0,
                wait_ms: [0, 0, 0],
            })
        );
    }

    #[test]
    fn a_lend_counts_as_giving_way() {
        let lines = "3000000\t3100000\ta\t1\t0\t10\tlend\n3100000\t3200000\tb\t1\t0\t11\tdone\n";
        assert_eq!(
            summarize(lines, &[], 4_000_000).map(|hour| hour.gave_way),
            Some(1)
        );
    }

    #[test]
    fn a_play_hand_over_counts_as_giving_way() {
        let lines = "3000000\t3100000\ta\t1\t0\t10\tplay\n3100000\t3200000\tb\t0\t0\t11\tdone\n3200000\t3300000\tc\t1\t0\t12\tsignal\n";
        assert_eq!(
            summarize(lines, &[], 4_000_000).map(|hour| hour.gave_way),
            Some(1)
        );
    }

    #[test]
    fn record_line_has_seven_columns() {
        assert_eq!(
            line(1, 2, "job", 3, 4, 5, "turn"),
            "1\t2\tjob\t3\t4\t5\tturn\n"
        );
    }

    #[test]
    fn cache_expires_after_ten_seconds() {
        let dir = test_dir("cache");
        let log = dir.join("log.tsv");
        assert_eq!(cached_json(&dir, &log, &[], 4_000_000), "null");
        fs::write(&log, "3990000\t3991000\ta\t1\t100\t42\tdone\n").unwrap();
        assert_eq!(cached_json(&dir, &log, &[], 4_009_999), "null");
        let refreshed = cached_json(&dir, &log, &[], 4_010_000);
        assert!(refreshed.contains("\"jobs\":1"));
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["hour", "log.tsv"]);
    }

    #[test]
    fn tail_and_trim_keep_whole_lines() {
        let path = test_dir("files").join("log.tsv");
        let mut body = "ab\n".repeat(12 * 1024 * 1024);
        body.push_str("last\n");
        fs::write(&path, body).unwrap();
        let tail = read_tail(&path).unwrap();
        assert!(tail.starts_with("ab\n"));
        assert!(tail.ends_with("last\n"));
        assert!(tail.len() <= 512 * 1024);
        trim(&path).unwrap();
        let kept = fs::read_to_string(&path).unwrap();
        assert!(kept.starts_with("ab\n"));
        assert!(kept.ends_with("last\n"));
        assert!(kept.len() <= 16 * 1024 * 1024);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(names.len(), 1);
    }
}
