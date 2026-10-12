use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cache;
use crate::paths::{
    boot_secs, clock_ticks, millis, proc_stat, start_ms, stat_ppid, stat_start, write_atomic,
};
use crate::queue::clean;
use crate::report::quote;
use crate::usage::{self, shading_ns};

const MIN_PCT: f64 = 5.0;

const FRESH_MS: u128 = 500;

const STALE_MS: u128 = 10_000;

const SWEEP_MS: u128 = 5_000;

const WALK_STEPS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) group: String,
    pub(crate) worker: Option<u32>,
    pub(crate) path: Option<String>,
    pub(crate) started: Option<u128>,
}

impl Lane {
    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"group\":{},\"worker\":{},\"path\":{},\"started\":{}}}",
            quote(&self.group),
            self.worker.map_or("null".to_string(), |w| w.to_string()),
            self.path.as_deref().map_or("null".to_string(), quote),
            self.started.map_or("null".to_string(), |s| s.to_string())
        )
    }

    pub(crate) fn text(&self) -> String {
        match self.worker {
            Some(worker) => format!("{} {worker}", self.group),
            None => self.group.clone(),
        }
    }
}

pub(crate) fn lane_json(lane: Option<&Lane>) -> String {
    lane.map_or("null".to_string(), Lane::json)
}

fn var<'a>(environ: &'a [u8], key: &str) -> Option<&'a [u8]> {
    environ.split(|b| *b == 0).find_map(|pair| {
        pair.strip_prefix(key.as_bytes())?
            .strip_prefix(b"=".as_slice())
    })
}

fn tidy(text: &str) -> String {
    clean(text.trim())
}

pub(crate) fn lane_of(environ: &[u8]) -> Option<Lane> {
    let group = tidy(&String::from_utf8_lossy(var(environ, "PUEUE_GROUP")?));
    if group.is_empty() {
        return None;
    }
    let worker = var(environ, "PUEUE_WORKER_ID")
        .and_then(|v| std::str::from_utf8(v).ok()?.trim().parse().ok());
    Some(Lane {
        group,
        worker,
        path: None,
        started: None,
    })
}

fn comm_of(proc_root: &Path, pid: u32) -> Option<String> {
    Some(tidy(
        &fs::read_to_string(proc_root.join(pid.to_string()).join("comm")).ok()?,
    ))
}

fn task_shell(proc_root: &Path, pid: u32) -> Option<(u32, String)> {
    let mut seen = BTreeSet::new();
    let mut current = pid;
    for _ in 0..WALK_STEPS {
        if !seen.insert(current) {
            return None;
        }
        let stat = proc_stat(proc_root, current)?;
        let parent = stat_ppid(&stat)?;
        if parent == 0 {
            return None;
        }
        if comm_of(proc_root, parent).as_deref() == Some("pueued") {
            return Some((current, stat));
        }
        current = parent;
    }
    None
}

fn live_path(raw: &str) -> String {
    raw.strip_suffix(" (deleted)").unwrap_or(raw).to_string()
}

fn shell_facts(proc_root: &Path, pid: u32, per_second: u64) -> (Option<String>, Option<u128>) {
    let Some((shell, stat)) = task_shell(proc_root, pid) else {
        return (None, None);
    };
    let path = fs::read_link(proc_root.join(shell.to_string()).join("cwd"))
        .ok()
        .map(|p| live_path(&p.to_string_lossy()));
    let started = stat_start(&stat)
        .zip(boot_secs(proc_root))
        .map(|(ticks, btime)| start_ms(ticks, per_second, btime));
    (path, started)
}

fn lane_full(proc_root: &Path, pid: u32, environ: &[u8], per_second: u64) -> Option<Lane> {
    let mut lane = lane_of(environ)?;
    (lane.path, lane.started) = shell_facts(proc_root, pid, per_second);
    Some(lane)
}

pub(crate) fn lanes(dir: &Path, jobs: &[(u32, u64)]) -> BTreeMap<u32, Option<Lane>> {
    lanes_in(Path::new("/proc"), dir, jobs, clock_ticks())
}

fn lane_with(proc_root: &Path, pid: u32, per_second: u64) -> Option<Lane> {
    let environ = fs::read(proc_root.join(pid.to_string()).join("environ")).ok()?;
    lane_full(proc_root, pid, &environ, per_second)
}

struct Saved {
    pid: u32,
    start: u64,
    lane: Option<Lane>,
}

fn plain(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

fn lanes_body(saved: &[Saved]) -> String {
    let mut out = String::new();
    for row in saved {
        match &row.lane {
            None => out.push_str(&format!("{}\t{}\t0\n", row.pid, row.start)),
            Some(lane) => out.push_str(&format!(
                "{}\t{}\t1\t{}\t{}\t{}\t{}\n",
                row.pid,
                row.start,
                plain(&lane.group),
                opt(lane.worker),
                opt(lane.path.as_deref().map(plain)),
                opt(lane.started)
            )),
        }
    }
    out
}

fn parse_saved(line: &str) -> Option<Saved> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() < 3 {
        return None;
    }
    let lane = if f[2] == "0" {
        None
    } else if f[2] == "1" && f.len() >= 7 {
        Some(Lane {
            group: f[3].to_string(),
            worker: f[4].parse().ok(),
            path: (f[5] != "-").then(|| f[5].to_string()),
            started: f[6].parse().ok(),
        })
    } else {
        return None;
    };
    Some(Saved {
        pid: f[0].parse().ok()?,
        start: f[1].parse().ok()?,
        lane,
    })
}

fn read_lanes(path: &Path) -> Vec<Saved> {
    fs::read_to_string(path)
        .ok()
        .map(|body| body.lines().filter_map(parse_saved).collect())
        .unwrap_or_default()
}

fn lanes_in(
    proc_root: &Path,
    dir: &Path,
    jobs: &[(u32, u64)],
    per_second: u64,
) -> BTreeMap<u32, Option<Lane>> {
    let path = dir.join("lanes");
    let saved = read_lanes(&path);
    let mut kept = Vec::new();
    let mut read = false;
    for &(pid, start) in jobs {
        let lane = match saved
            .iter()
            .find(|row| row.pid == pid && row.start == start)
        {
            Some(row) => row.lane.clone(),
            None => {
                read = true;
                lane_with(proc_root, pid, per_second)
            }
        };
        kept.push(Saved { pid, start, lane });
    }
    kept.sort_by_key(|row| (row.pid, row.start));
    if (read || kept.len() != saved.len()) && dir.is_dir() {
        let _ = write_atomic(&path, &lanes_body(&kept));
    }
    kept.into_iter().map(|row| (row.pid, row.lane)).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) pid: u32,
    start: u64,
    pub(crate) comm: String,
    clients: Vec<(u64, u64)>,
    fds: Vec<u32>,
    swept: Option<u128>,
    pub(crate) vram_kib: u64,
    pub(crate) lane: Option<Lane>,
    pub(crate) since: Option<u128>,
    pub(crate) pct: Option<u32>,
}

impl Row {
    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"pid\":{},\"comm\":{},\"gpu_pct\":{},\"vram_mib\":{},\"lane\":{},\"since\":{}}}",
            self.pid,
            quote(&self.comm),
            self.pct.unwrap_or(0),
            self.vram_kib / 1024,
            lane_json(self.lane.as_ref()),
            self.since.unwrap_or(0)
        )
    }

    pub(crate) fn text(&self) -> String {
        format!(
            "outside  {} {}  {}% of the GPU outside pgpu ({})",
            self.comm,
            self.pid,
            self.pct.unwrap_or(0),
            self.lane
                .as_ref()
                .map_or("build.slice".to_string(), Lane::text)
        )
    }
}

fn slice_pids(slice: &Path) -> Vec<u32> {
    let mut pids = BTreeSet::new();
    let mut dirs = vec![slice.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if let Ok(body) = fs::read_to_string(dir.join("cgroup.procs")) {
            pids.extend(body.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            dirs.extend(
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path()),
            );
        }
    }
    pids.into_iter().collect()
}

struct Quiet {
    pid: u32,
    swept: u128,
    again: bool,
}

struct Memory {
    at: u128,
    rows: Vec<Row>,
    quiet: Vec<Quiet>,
    queued: Vec<u32>,
}

fn opt<T: ToString>(value: Option<T>) -> String {
    value.map_or("-".to_string(), |v| v.to_string())
}

fn due(swept: Option<u128>, now: u128) -> bool {
    swept.is_none_or(|at| now.saturating_sub(at) >= SWEEP_MS)
}

fn start_of(proc_root: &Path, pid: u32) -> Option<u64> {
    proc_stat(proc_root, pid).and_then(|stat| stat_start(&stat))
}

fn take_client(clients: &mut BTreeMap<u64, usage::Client>, info: usage::FdInfo) {
    let vram = info.vram_kib.unwrap_or(0);
    let have = clients.entry(info.client).or_insert(usage::Client {
        engines: BTreeMap::new(),
        vram_kib: 0,
    });
    if info.vram_kib.is_some() {
        have.vram_kib = have.vram_kib.max(vram);
    }
    for (name, eng) in info.engines {
        have.engines
            .entry(name)
            .and_modify(|old| old.ns = old.ns.max(eng.ns))
            .or_insert(eng);
    }
}

fn read_info(path: &Path) -> Option<usage::FdInfo> {
    usage::parse_fdinfo(&fs::read_to_string(path).ok()?)
}

fn sample_fds(proc_root: &Path, pid: u32, fds: &[u32]) -> BTreeMap<u64, usage::Client> {
    let base = proc_root.join(pid.to_string()).join("fdinfo");
    let mut clients = BTreeMap::new();
    for fd in fds {
        if let Some(info) = read_info(&base.join(fd.to_string())) {
            take_client(&mut clients, info);
        }
    }
    clients
}

fn sweep_fds(proc_root: &Path, pid: u32) -> (BTreeMap<u64, usage::Client>, Vec<u32>) {
    let base = proc_root.join(pid.to_string());
    let mut clients = BTreeMap::new();
    let mut fds = Vec::new();
    let Ok(entries) = fs::read_dir(base.join("fd")) else {
        return (clients, fds);
    };
    for entry in entries.flatten() {
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        if fs::read_link(entry.path()).is_ok_and(|to| to.starts_with("/dev/dri/")) {
            fds.push(fd);
            if let Some(info) = read_info(&base.join("fdinfo").join(fd.to_string())) {
                take_client(&mut clients, info);
            }
        }
    }
    fds.sort_unstable();
    (clients, fds)
}

fn claim(
    found: &BTreeMap<u64, usage::Client>,
    claimed: &mut BTreeSet<u64>,
) -> (Vec<(u64, u64)>, u64) {
    let mut clients = Vec::new();
    let mut vram_kib = 0u64;
    for (id, client) in found {
        if claimed.insert(*id) {
            clients.push((*id, shading_ns(client)));
            vram_kib = vram_kib.saturating_add(client.vram_kib);
        }
    }
    (clients, vram_kib)
}

fn cache_body(at: u128, rows: &[Row], quiet: &[Quiet], queued: &[u32]) -> String {
    let mut out = format!("v2\n{at}\n");
    for row in rows {
        let clients: Vec<String> = row
            .clients
            .iter()
            .map(|(id, ns)| format!("{id}:{ns}"))
            .collect();
        let fds: Vec<String> = row.fds.iter().map(u32::to_string).collect();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.pid,
            row.start,
            opt(row.since),
            opt(row.pct),
            row.vram_kib,
            clients.join(","),
            opt(row.lane.as_ref().map(|l| l.group.clone())),
            opt(row.lane.as_ref().and_then(|l| l.worker)),
            opt(row
                .lane
                .as_ref()
                .and_then(|l| l.path.as_deref())
                .map(|p| p.replace(['\t', '\n'], " "))),
            opt(row.lane.as_ref().and_then(|l| l.started)),
            row.comm,
            fds.join(","),
            opt(row.swept)
        ));
    }
    for row in quiet {
        out.push_str(&format!(
            "Q\t{}\t{}\t{}\n",
            row.pid,
            row.swept,
            u8::from(row.again)
        ));
    }
    for pid in queued {
        out.push_str(&format!("G\t{pid}\n"));
    }
    out
}

fn parse_clients(field: &str) -> Vec<(u64, u64)> {
    field
        .split(',')
        .filter_map(|pair| {
            let (id, ns) = pair.split_once(':')?;
            Some((id.parse().ok()?, ns.parse().ok()?))
        })
        .collect()
}

fn parse_row(line: &str, versioned: bool) -> Option<Row> {
    let f: Vec<&str> = if versioned {
        line.split('\t').collect()
    } else {
        line.splitn(11, '\t').collect()
    };
    if f.len() < 11 {
        return None;
    }
    let fds = if versioned {
        f.get(11)
            .map(|field| field.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let swept = if versioned {
        f.get(12).and_then(|field| field.parse().ok())
    } else {
        None
    };
    Some(Row {
        pid: f[0].parse().ok()?,
        start: f[1].parse().ok()?,
        since: f[2].parse().ok(),
        pct: f[3].parse().ok(),
        vram_kib: f[4].parse().ok()?,
        clients: parse_clients(f[5]),
        fds,
        swept,
        lane: (f[6] != "-").then(|| Lane {
            group: f[6].to_string(),
            worker: f[7].parse().ok(),
            path: (f[8] != "-").then(|| f[8].to_string()),
            started: f[9].parse().ok(),
        }),
        comm: f[10].to_string(),
    })
}

fn parse_quiet(rest: &str) -> Option<Quiet> {
    let f: Vec<&str> = rest.split('\t').collect();
    if f.len() < 3 {
        return None;
    }
    Some(Quiet {
        pid: f[0].parse().ok()?,
        swept: f[1].parse().ok()?,
        again: f[2] == "1",
    })
}

fn parse_cache(body: &str) -> Option<Memory> {
    let mut lines = body.lines();
    let first = lines.next()?.trim();
    let (at, versioned) = if first == "v2" {
        (lines.next()?.trim().parse().ok()?, true)
    } else {
        (first.parse().ok()?, false)
    };
    let mut rows = Vec::new();
    let mut quiet = Vec::new();
    let mut queued = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if versioned && let Some(rest) = line.strip_prefix("Q\t") {
            if let Some(row) = parse_quiet(rest) {
                quiet.push(row);
            }
            continue;
        }
        if versioned && let Some(rest) = line.strip_prefix("G\t") {
            if let Ok(pid) = rest.trim().parse() {
                queued.push(pid);
            }
            continue;
        }
        if let Some(row) = parse_row(line, versioned) {
            rows.push(row);
        }
    }
    Some(Memory {
        at,
        rows,
        quiet,
        queued,
    })
}

fn counted(rows: Vec<Row>) -> Vec<Row> {
    rows.into_iter().filter(|r| r.since.is_some()).collect()
}

fn remember(quiet: &mut Vec<Quiet>, pid: u32, swept: u128, again: bool) {
    quiet.push(Quiet { pid, swept, again });
}

fn outside_in(proc_root: &Path, slice: &Path, cache: &Path, now: u128) -> Vec<Row> {
    let cached = fs::read_to_string(cache)
        .ok()
        .and_then(|body| parse_cache(&body))
        .filter(|mem| cache::fresh(mem.at, now, STALE_MS + 1));
    if let Some(mem) = &cached
        && cache::fresh(mem.at, now, FRESH_MS)
    {
        return counted(mem.rows.clone());
    }
    let per_second = clock_ticks();
    let old_rows = cached
        .as_ref()
        .map(|mem| mem.rows.as_slice())
        .unwrap_or(&[]);
    let old_quiet = cached
        .as_ref()
        .map(|mem| mem.quiet.as_slice())
        .unwrap_or(&[]);
    let old_queued: BTreeSet<u32> = cached
        .as_ref()
        .map(|mem| mem.queued.iter().copied().collect())
        .unwrap_or_default();
    let mut claimed = BTreeSet::new();
    let mut rows = Vec::new();
    let mut quiet = Vec::new();
    let mut queued = Vec::new();
    for pid in slice_pids(slice) {
        if old_queued.contains(&pid) {
            queued.push(pid);
            continue;
        }
        let rested = old_quiet.iter().find(|row| row.pid == pid);
        if let Some(row) = rested
            && !row.again
            && now.saturating_sub(row.swept) < SWEEP_MS
        {
            remember(&mut quiet, pid, row.swept, false);
            continue;
        }
        let known = old_rows
            .iter()
            .find(|row| row.pid == pid)
            .filter(|row| start_of(proc_root, pid) == Some(row.start));
        let (found, fds, swept) = match known {
            Some(row) if !row.fds.is_empty() && !due(row.swept, now) => (
                sample_fds(proc_root, pid, &row.fds),
                row.fds.clone(),
                row.swept.unwrap_or(now),
            ),
            _ => {
                let (found, fds) = sweep_fds(proc_root, pid);
                (found, fds, now)
            }
        };
        if found.is_empty() {
            let opening = known.is_none() && rested.is_none();
            remember(&mut quiet, pid, swept, opening);
            continue;
        }
        let (start, comm, lane) = match known {
            Some(row) => (row.start, row.comm.clone(), row.lane.clone()),
            None => {
                let Ok(environ) = fs::read(proc_root.join(pid.to_string()).join("environ")) else {
                    continue;
                };
                if var(&environ, "GPU_QUEUE").is_some() {
                    queued.push(pid);
                    continue;
                }
                let Some(start) = start_of(proc_root, pid) else {
                    continue;
                };
                (
                    start,
                    comm_of(proc_root, pid).unwrap_or_default(),
                    lane_full(proc_root, pid, &environ, per_second),
                )
            }
        };
        let (clients, vram_kib) = claim(&found, &mut claimed);
        rows.push(Row {
            pid,
            start,
            comm,
            clients,
            fds,
            swept: Some(swept),
            vram_kib,
            lane,
            since: None,
            pct: None,
        });
    }
    if let Some(mem) = &cached {
        let elapsed_ns = (now - mem.at) as f64 * 1e6;
        for row in &mut rows {
            let Some(was) = mem
                .rows
                .iter()
                .find(|old| old.pid == row.pid && old.start == row.start)
            else {
                continue;
            };
            let delta: u64 = row
                .clients
                .iter()
                .map(|(id, ns)| {
                    was.clients
                        .iter()
                        .find(|(then_id, _)| then_id == id)
                        .map_or(0, |(_, then)| ns.saturating_sub(*then))
                })
                .sum();
            let pct = (delta as f64 / elapsed_ns * 100.0).clamp(0.0, 100.0);
            row.pct = Some(pct.round() as u32);
            row.since = (pct >= MIN_PCT).then(|| was.since.unwrap_or(now));
        }
    }
    quiet.sort_by_key(|row| row.pid);
    let _ = write_atomic(cache, &cache_body(now, &rows, &quiet, &queued));
    counted(rows)
}

fn build_slice() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/build.slice"
    ))
}

pub(crate) fn outside(dir: &Path) -> Vec<Row> {
    outside_in(
        Path::new("/proc"),
        &build_slice(),
        &dir.join("outside"),
        millis(),
    )
}

pub(crate) fn json(rows: &[Row]) -> String {
    let items: Vec<String> = rows.iter().map(Row::json).collect();
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::{Lane, json, lane_of, lane_with, lanes_in, live_path, outside_in};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-outside-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    struct Tree {
        proc_root: PathBuf,
        slice: PathBuf,
        cache: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Self {
            let root = test_dir(name);
            let tree = Self {
                proc_root: root.join("proc"),
                slice: root.join("build.slice"),
                cache: root.join("outside"),
            };
            fs::create_dir_all(tree.slice.join("pueued.service")).unwrap();
            fs::create_dir_all(tree.slice.join("run-p1.scope")).unwrap();
            tree
        }

        fn process(&self, pid: u32, comm: &str, env: &[&str], start: u64) {
            let base = self.proc_root.join(pid.to_string());
            fs::create_dir_all(base.join("fd")).unwrap();
            fs::create_dir_all(base.join("fdinfo")).unwrap();
            let mut environ = Vec::new();
            for pair in env {
                environ.extend_from_slice(pair.as_bytes());
                environ.push(0);
            }
            fs::write(base.join("environ"), environ).unwrap();
            fs::write(base.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(
                base.join("stat"),
                format!("{pid} ({comm}) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 1 1\n"),
            )
            .unwrap();
            let _ = fs::remove_file(base.join("fd").join("1"));
            symlink("/dev/null", base.join("fd").join("1")).unwrap_or(());
        }

        fn parent(&self, pid: u32, ppid: u32) {
            let base = self.proc_root.join(pid.to_string());
            let comm = fs::read_to_string(base.join("comm")).unwrap();
            let start = fs::read_to_string(base.join("stat"))
                .ok()
                .and_then(|s| crate::paths::stat_start(&s))
                .unwrap_or(1);
            fs::write(
                base.join("stat"),
                format!(
                    "{pid} ({}) S {ppid} 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 1 1\n",
                    comm.trim()
                ),
            )
            .unwrap();
        }

        fn cwd(&self, pid: u32, dir: &str) {
            let link = self.proc_root.join(pid.to_string()).join("cwd");
            let _ = fs::remove_file(&link);
            symlink(dir, link).unwrap();
        }

        fn boot(&self, btime: u64) {
            fs::create_dir_all(&self.proc_root).unwrap();
            fs::write(
                self.proc_root.join("stat"),
                format!("cpu  1 2 3\nbtime {btime}\nprocesses 9\n"),
            )
            .unwrap();
        }

        fn gpu(&self, pid: u32, fd: u32, client: u64, gfx_ns: u64, compute_ns: u64, vram_kib: u64) {
            let base = self.proc_root.join(pid.to_string());
            let link = base.join("fd").join(fd.to_string());
            let _ = fs::remove_file(&link);
            symlink("/dev/dri/renderD128", &link).unwrap();
            self.info(pid, fd, client, gfx_ns, compute_ns, vram_kib);
        }

        fn info(
            &self,
            pid: u32,
            fd: u32,
            client: u64,
            gfx_ns: u64,
            compute_ns: u64,
            vram_kib: u64,
        ) {
            let base = self.proc_root.join(pid.to_string());
            fs::write(
                base.join("fdinfo").join(fd.to_string()),
                format!(
                    "pos:\t0\ndrm-driver:\tamdgpu\ndrm-client-id:\t{client}\n\
                     drm-memory-vram:\t{vram_kib} KiB\ndrm-engine-gfx:\t{gfx_ns} ns\n\
                     drm-engine-compute:\t{compute_ns} ns\ndrm-engine-enc:\t999999999999 ns\n"
                ),
            )
            .unwrap();
        }

        fn queue(&self) -> PathBuf {
            self.cache.parent().unwrap().join("queue")
        }

        fn in_slice(&self, cgroup: &str, pids: &[u32]) {
            let body: Vec<String> = pids.iter().map(u32::to_string).collect();
            fs::write(
                self.slice.join(cgroup).join("cgroup.procs"),
                body.join("\n") + "\n",
            )
            .unwrap();
        }

        fn look(&self, now: u128) -> Vec<(u32, u32, Option<u128>)> {
            outside_in(&self.proc_root, &self.slice, &self.cache, now)
                .iter()
                .map(|r| (r.pid, r.pct.unwrap(), r.since))
                .collect()
        }
    }

    #[test]
    fn a_lane_comes_from_pueues_environment() {
        assert_eq!(
            lane_of(b"A=1\0PUEUE_GROUP=heavy\0PUEUE_WORKER_ID=1\0"),
            Some(Lane {
                group: "heavy".to_string(),
                worker: Some(1),
                path: None,
                started: None
            })
        );
        assert_eq!(
            lane_of(b"PUEUE_GROUP=gpu\0PUEUE_WORKER_ID=x\0"),
            Some(Lane {
                group: "gpu".to_string(),
                worker: None,
                path: None,
                started: None
            })
        );
        assert_eq!(lane_of(b"PUEUE_WORKER_ID=1\0"), None);
        assert_eq!(lane_of(b"PUEUE_GROUP=\0"), None);
        assert_eq!(lane_of(b"XPUEUE_GROUP=heavy\0PUEUE_GROUPS=x\0"), None);
        assert_eq!(lane_of(b""), None);
        let lane = lane_of(b"PUEUE_GROUP=render\0PUEUE_WORKER_ID=0").unwrap();
        assert_eq!(
            lane.json(),
            "{\"group\":\"render\",\"worker\":0,\"path\":null,\"started\":null}"
        );
        assert_eq!(lane.text(), "render 0");
        let tree = Tree::new("lane");
        tree.process(10, "pgpu", &["PUEUE_GROUP=gpu", "PUEUE_WORKER_ID=0"], 1);
        tree.process(11, "pgpu", &["HOME=/x"], 1);
        assert_eq!(
            lane_with(&tree.proc_root, 10, 100)
                .map(|l| l.json())
                .as_deref(),
            Some("{\"group\":\"gpu\",\"worker\":0,\"path\":null,\"started\":null}")
        );
        assert_eq!(lane_with(&tree.proc_root, 11, 100), None);
        assert_eq!(lane_with(&tree.proc_root, 12, 100), None);
    }

    #[test]
    fn a_task_folder_removed_under_its_shell_keeps_its_path_without_the_deleted_mark() {
        let tree = Tree::new("deleted");
        tree.boot(1_700_000_000);
        tree.process(50, "pueued", &[], 1);
        tree.parent(50, 1);
        tree.process(60, "sh", &[], 12_345);
        tree.parent(60, 50);
        tree.cwd(60, "/work/tmp/wt/gone (deleted)");
        tree.process(
            70,
            "pgpu",
            &["PUEUE_GROUP=heavy", "PUEUE_WORKER_ID=0"],
            12_400,
        );
        tree.parent(70, 60);
        let lane = lane_with(&tree.proc_root, 70, 100).unwrap();
        assert_eq!(lane.path.as_deref(), Some("/work/tmp/wt/gone"));
        assert_eq!(live_path("/a (deleted) b"), "/a (deleted) b");
        assert_eq!(live_path("/plain"), "/plain");
    }

    #[test]
    fn a_lane_names_the_pueue_shell_below_pueued_by_path_and_start() {
        let tree = Tree::new("shell");
        tree.boot(1_700_000_000);
        tree.process(50, "pueued", &[], 1);
        tree.parent(50, 1);
        tree.process(60, "sh", &[], 12_345);
        tree.parent(60, 50);
        tree.cwd(60, "/work/job-a");
        tree.process(
            70,
            "pgpu",
            &["PUEUE_GROUP=gpu", "PUEUE_WORKER_ID=1"],
            12_400,
        );
        tree.parent(70, 60);
        tree.cwd(70, "/elsewhere");
        tree.process(
            80,
            "blender",
            &["PUEUE_GROUP=gpu", "PUEUE_WORKER_ID=1"],
            12_500,
        );
        tree.parent(80, 70);
        let lane = lane_with(&tree.proc_root, 80, 100).unwrap();
        assert_eq!(lane.path.as_deref(), Some("/work/job-a"));
        assert_eq!(lane.started, Some(1_700_000_123_450));
        assert_eq!(
            lane.json(),
            "{\"group\":\"gpu\",\"worker\":1,\"path\":\"/work/job-a\",\"started\":1700000123450}"
        );
        assert_eq!(lane.text(), "gpu 1");
        let wrapper = lane_with(&tree.proc_root, 70, 250).unwrap();
        assert_eq!(wrapper.path.as_deref(), Some("/work/job-a"));
        assert_eq!(wrapper.started, Some(1_700_000_049_380));
        tree.parent(60, 1);
        tree.parent(70, 50);
        let direct = lane_with(&tree.proc_root, 80, 100).unwrap();
        assert_eq!(
            direct.path.as_deref(),
            Some("/elsewhere"),
            "the wrapper itself when pueued is its parent"
        );
        assert_eq!(direct.started, Some(1_700_000_124_000));
    }

    #[test]
    fn a_lane_without_pueued_above_it_has_null_path_and_started() {
        let tree = Tree::new("noshell");
        tree.boot(1_700_000_000);
        tree.process(60, "sh", &[], 100);
        tree.parent(60, 1);
        tree.cwd(60, "/work");
        tree.process(70, "pgpu", &["PUEUE_GROUP=gpu"], 200);
        tree.parent(70, 60);
        let lane = lane_with(&tree.proc_root, 70, 100).unwrap();
        assert_eq!((lane.path, lane.started), (None, None));
        assert_eq!(
            lane_with(&tree.proc_root, 70, 100).unwrap().json(),
            "{\"group\":\"gpu\",\"worker\":null,\"path\":null,\"started\":null}"
        );
        tree.process(50, "pueued", &[], 1);
        tree.parent(50, 1);
        tree.parent(60, 50);
        assert!(lane_with(&tree.proc_root, 70, 100).unwrap().path.is_some());
        fs::remove_file(tree.proc_root.join("stat")).unwrap();
        let lane = lane_with(&tree.proc_root, 70, 100).unwrap();
        assert_eq!(lane.path.as_deref(), Some("/work"));
        assert_eq!(lane.started, None, "no btime, no start");
    }

    #[test]
    fn the_walk_ends_on_a_loop_a_missing_parent_or_too_many_steps() {
        let tree = Tree::new("walk");
        tree.boot(1_700_000_000);
        tree.process(10, "a", &["PUEUE_GROUP=gpu"], 1);
        tree.process(11, "b", &[], 1);
        tree.parent(10, 11);
        tree.parent(11, 10);
        let lane = lane_with(&tree.proc_root, 10, 100).unwrap();
        assert_eq!((lane.path, lane.started), (None, None), "a loop");
        tree.parent(11, 999);
        let lane = lane_with(&tree.proc_root, 10, 100).unwrap();
        assert_eq!((lane.path, lane.started), (None, None), "a missing parent");
        tree.parent(11, 0);
        let lane = lane_with(&tree.proc_root, 10, 100).unwrap();
        assert_eq!((lane.path, lane.started), (None, None), "the root");
        tree.process(1000, "pueued", &[], 1);
        tree.parent(1000, 1);
        for pid in 2000..2070 {
            tree.process(pid, "sh", &[], 1);
            tree.parent(pid, if pid == 2069 { 1000 } else { pid + 1 });
            tree.cwd(pid, "/deep");
        }
        tree.process(3000, "pgpu", &["PUEUE_GROUP=gpu"], 1);
        tree.parent(3000, 2000);
        let lane = lane_with(&tree.proc_root, 3000, 100).unwrap();
        assert_eq!(
            (lane.path, lane.started),
            (None, None),
            "70 steps up is past the limit"
        );
        tree.parent(3000, 2010);
        let lane = lane_with(&tree.proc_root, 3000, 100).unwrap();
        assert_eq!(
            lane.path.as_deref(),
            Some("/deep"),
            "60 steps up is within it"
        );
    }

    #[test]
    fn an_outside_row_carries_its_shell_through_the_cache() {
        let tree = Tree::new("rowshell");
        tree.boot(1_700_000_000);
        tree.process(50, "pueued", &[], 1);
        tree.parent(50, 1);
        tree.process(60, "sh", &[], 12_345);
        tree.parent(60, 50);
        tree.cwd(60, "/work/job b");
        tree.process(
            100,
            "blender",
            &["PUEUE_GROUP=heavy", "PUEUE_WORKER_ID=1"],
            7,
        );
        tree.parent(100, 60);
        tree.gpu(100, 5, 1, 0, 0, 2048 * 1024);
        tree.in_slice("pueued.service", &[100]);
        let t = 1_000_000u128;
        assert!(tree.look(t).is_empty());
        tree.gpu(100, 5, 1, 300_000_000, 0, 2048 * 1024);
        let rows = outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 1_000);
        let want = rows[0].lane.clone().unwrap();
        assert_eq!(want.path.as_deref(), Some("/work/job b"));
        assert!(want.started.is_some());
        let again = outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 1_100);
        assert_eq!(
            again[0].lane.as_ref(),
            Some(&want),
            "read back from the cache"
        );
        assert_eq!(json(&again), json(&rows));
    }

    #[test]
    fn outside_counts_build_lane_gpu_users_above_5_percent_once_per_client() {
        let tree = Tree::new("delta");
        tree.process(
            100,
            "blender",
            &["PUEUE_GROUP=heavy", "PUEUE_WORKER_ID=1"],
            7,
        );
        tree.gpu(100, 5, 1, 0, 0, 2048 * 1024);
        tree.gpu(100, 6, 1, 0, 0, 2048 * 1024);
        tree.process(101, "blender-kid", &["PUEUE_GROUP=heavy"], 8);
        tree.gpu(101, 3, 1, 0, 0, 2048 * 1024);
        tree.process(200, "render", &["PUEUE_GROUP=gpu", "GPU_QUEUE=1"], 9);
        tree.gpu(200, 4, 2, 0, 0, 100);
        tree.process(300, "cc", &[], 10);
        tree.gpu(300, 4, 3, 0, 0, 1024);
        tree.process(400, "app", &[], 11);
        tree.gpu(400, 4, 4, 0, 0, 1024);
        tree.process(500, "rustc", &["PUEUE_GROUP=heavy"], 12);
        tree.in_slice("pueued.service", &[100, 101, 200, 500]);
        tree.in_slice("run-p1.scope", &[300]);
        let t = 1_000_000u128;
        assert!(
            tree.look(t).is_empty(),
            "no cache: seeds and reports nothing"
        );
        assert!(tree.cache.exists());
        tree.gpu(100, 5, 1, 300_000_000, 40_000_000, 2048 * 1024);
        tree.gpu(100, 6, 1, 300_000_000, 40_000_000, 2048 * 1024);
        tree.gpu(101, 3, 1, 300_000_000, 40_000_000, 2048 * 1024);
        tree.gpu(200, 4, 2, 900_000_000, 0, 100);
        tree.gpu(300, 4, 3, 40_000_000, 0, 1024);
        tree.gpu(400, 4, 4, 900_000_000, 0, 1024);
        assert_eq!(
            tree.look(t + 1_000),
            vec![(100, 34, Some(t + 1_000))],
            "4 % stays out, a queued job and a process outside the lane are not counted"
        );
        let rows = outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 1_200);
        assert_eq!(
            rows.len(),
            1,
            "a cache under 0.5 s old is reported as it is"
        );
        assert_eq!(
            json(&rows),
            format!(
                "[{{\"pid\":100,\"comm\":\"blender\",\"gpu_pct\":34,\"vram_mib\":2048,\
                 \"lane\":{{\"group\":\"heavy\",\"worker\":1,\"path\":null,\"started\":null}},\
                 \"since\":{}}}]",
                t + 1_000
            )
        );
        assert_eq!(
            rows[0].text(),
            "outside  blender 100  34% of the GPU outside pgpu (heavy 1)"
        );
        tree.gpu(100, 5, 1, 800_000_000, 40_000_000, 2048 * 1024);
        tree.gpu(300, 4, 3, 140_000_000, 0, 1024);
        assert_eq!(
            tree.look(t + 2_000),
            vec![(100, 50, Some(t + 1_000)), (300, 10, Some(t + 2_000))],
            "since stays at the first crossing"
        );
        let text = outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 2_100);
        assert_eq!(
            text[1].text(),
            "outside  cc 300  10% of the GPU outside pgpu (build.slice)"
        );
        tree.gpu(300, 4, 3, 141_000_000, 0, 1024);
        assert_eq!(
            tree.look(t + 3_000),
            vec![],
            "a quiet process drops out and a quiet second clears since"
        );
        tree.gpu(100, 5, 1, 1_300_000_000, 40_000_000, 2048 * 1024);
        assert_eq!(tree.look(t + 4_000), vec![(100, 50, Some(t + 4_000))]);
        tree.gpu(100, 5, 1, 9_300_000_000, 40_000_000, 2048 * 1024);
        assert!(
            tree.look(t + 15_000).is_empty(),
            "a cache over 10 s old reseeds"
        );
        tree.gpu(100, 5, 1, 9_800_000_000, 40_000_000, 2048 * 1024);
        assert_eq!(tree.look(t + 16_000), vec![(100, 50, Some(t + 16_000))]);
        tree.process(
            100,
            "blender",
            &["PUEUE_GROUP=heavy", "PUEUE_WORKER_ID=1"],
            99,
        );
        tree.gpu(100, 5, 1, 9_900_000_000, 40_000_000, 2048 * 1024);
        assert!(
            tree.look(t + 17_000).is_empty(),
            "a reused pid is not compared with the process before it"
        );
        assert!(
            tree.look(t).is_empty(),
            "a cache from the future reseeds instead of dividing by a negative time"
        );
    }

    #[test]
    fn a_process_with_no_gpu_or_no_readable_environ_is_skipped() {
        let tree = Tree::new("skip");
        tree.process(1, "make", &["PUEUE_GROUP=heavy"], 1);
        tree.in_slice("pueued.service", &[1, 2]);
        let t = 5_000u128;
        assert!(tree.look(t).is_empty());
        assert!(tree.look(t + 1_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(cache.starts_with(&format!("v2\n{}\n", t + 1_000)));
        assert!(cache.contains(&format!("Q\t1\t{}\t0\n", t + 1_000)));
        assert!(!cache.lines().any(|line| line.starts_with("1\t")));
        assert!(Path::new(&tree.cache).exists());
    }

    struct OpenFd(PathBuf);

    impl Drop for OpenFd {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn a_known_gpu_user_is_sampled_from_its_drm_fds() {
        let tree = Tree::new("sample");
        tree.process(100, "blender", &["PUEUE_GROUP=heavy"], 7);
        tree.gpu(100, 5, 1, 0, 0, 1024 * 1024);
        tree.in_slice("pueued.service", &[100]);
        let t = 1_000_000u128;
        assert!(tree.look(t).is_empty());
        tree.info(100, 7, 1, 9_000_000_000, 0, 1024 * 1024);
        let fd = tree.proc_root.join("100").join("fd");
        let rows = {
            let _open = OpenFd(fd.clone());
            fs::set_permissions(&fd, fs::Permissions::from_mode(0o0)).unwrap();
            tree.info(100, 5, 1, 250_000_000, 0, 1024 * 1024);
            outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 1_000)
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pct, Some(25));
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.contains(&format!("blender\t5\t{t}\n")),
            "the other fdinfo was not read and the fd dir was not swept"
        );
        tree.gpu(100, 9, 2, 0, 0, 1024);
        assert!(
            tree.look(t + 6_000).is_empty(),
            "a new client has no delta yet"
        );
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.contains("5,9") || cache.contains("9,5"),
            "a known user is swept again once the sweep period has passed: {cache}"
        );
        tree.info(100, 9, 2, 400_000_000, 0, 1024);
        assert_eq!(
            tree.look(t + 7_000),
            vec![(100, 40, Some(t + 7_000))],
            "the fd opened between sweeps counts on the next sample"
        );
    }

    #[test]
    fn a_quiet_pid_is_not_swept_again_within_5_seconds() {
        let tree = Tree::new("quiet");
        tree.process(10, "wineserver", &[], 3);
        tree.in_slice("pueued.service", &[10]);
        let t = 2_000_000u128;
        assert!(tree.look(t).is_empty());
        assert!(tree.look(t + 1_000).is_empty());
        tree.gpu(10, 4, 1, 500_000_000, 0, 1024);
        assert!(tree.look(t + 2_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.contains(&format!("Q\t10\t{}\t0\n", t + 1_000)),
            "still the sweep from the previous rebuild: {cache}"
        );
        assert!(!cache.lines().any(|line| line.starts_with("10\t")));
        assert!(
            tree.look(t + 6_000).is_empty(),
            "the finding sweep has no delta yet"
        );
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.lines().any(|line| line.starts_with("10\t3\t")),
            "swept once the last one is 5 s old: {cache}"
        );
        assert!(!cache.contains("Q\t10\t"));
        tree.info(10, 4, 1, 1_000_000_000, 0, 1024);
        assert_eq!(tree.look(t + 7_000), vec![(10, 50, Some(t + 7_000))]);
    }

    #[test]
    fn a_new_pid_is_swept_at_once_and_once_more_at_the_next_rebuild() {
        let tree = Tree::new("new");
        tree.process(20, "app", &[], 4);
        tree.in_slice("pueued.service", &[20]);
        let t = 3_000_000u128;
        assert!(tree.look(t).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(cache.contains(&format!("Q\t20\t{t}\t1\n")));
        tree.gpu(20, 3, 7, 0, 0, 2048);
        assert!(
            tree.look(t + 1_000).is_empty(),
            "the second sweep finds the fd but has no earlier sample"
        );
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.lines().any(|line| line.starts_with("20\t4\t")),
            "the fd opened after the first sweep is in the cache: {cache}"
        );
        assert!(!cache.contains("Q\t20\t"));
        tree.info(20, 3, 7, 500_000_000, 0, 2048);
        assert_eq!(tree.look(t + 2_000), vec![(20, 50, Some(t + 2_000))]);
    }

    #[test]
    fn a_pid_that_left_is_dropped_and_a_reused_quiet_pid_is_swept_at_its_period() {
        let tree = Tree::new("reuse");
        tree.process(30, "old", &[], 5);
        tree.process(31, "gone", &[], 6);
        tree.in_slice("pueued.service", &[30, 31]);
        let t = 4_000_000u128;
        assert!(tree.look(t).is_empty());
        assert!(tree.look(t + 1_000).is_empty());
        tree.in_slice("pueued.service", &[30]);
        tree.process(30, "old", &[], 99);
        tree.gpu(30, 2, 4, 0, 0, 1024);
        assert!(tree.look(t + 2_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            !cache.contains("31"),
            "a pid that left the slice is dropped: {cache}"
        );
        assert!(
            cache.contains(&format!("Q\t30\t{}\t0\n", t + 1_000)),
            "a quiet pid is bare, so its reuse waits for the sweep: {cache}"
        );
        assert!(tree.look(t + 6_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.contains("30\t99\t"),
            "the sweep finds the new process under its start: {cache}"
        );
        assert!(!cache.contains("Q\t30\t"));
    }

    #[test]
    fn a_quiet_pid_costs_no_read_until_its_sweep_is_due() {
        let tree = Tree::new("quietread");
        tree.process(40, "wineserver", &[], 8);
        tree.in_slice("pueued.service", &[40]);
        let t = 6_000_000u128;
        assert!(tree.look(t).is_empty());
        assert!(tree.look(t + 1_000).is_empty());
        fs::remove_dir_all(tree.proc_root.join("40")).unwrap();
        assert!(tree.look(t + 2_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(
            cache.contains(&format!("Q\t40\t{}\t0\n", t + 1_000)),
            "kept with its sweep time, read neither for its stat nor its fds: {cache}"
        );
    }

    #[test]
    fn a_queued_gpu_user_is_skipped_for_life_without_reading_its_environ_again() {
        let tree = Tree::new("queued");
        tree.process(
            50,
            "cargo-test",
            &["GPU_QUEUE=/run/q", "PUEUE_GROUP=gpu"],
            9,
        );
        tree.gpu(50, 3, 2, 0, 0, 1024);
        tree.in_slice("pueued.service", &[50]);
        let t = 7_000_000u128;
        assert!(tree.look(t).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(cache.contains("G\t50\n"), "{cache}");
        fs::remove_file(tree.proc_root.join("50").join("environ")).unwrap();
        tree.info(50, 3, 2, 900_000_000, 0, 1024);
        assert!(tree.look(t + 1_000).is_empty());
        assert!(tree.look(t + 7_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(cache.contains("G\t50\n"), "{cache}");
        assert!(!cache.contains("Q\t50\t"));
        tree.in_slice("pueued.service", &[]);
        assert!(tree.look(t + 8_000).is_empty());
        let cache = fs::read_to_string(&tree.cache).unwrap();
        assert!(!cache.contains("G\t50"), "dropped once it left: {cache}");
    }

    #[test]
    fn an_old_outside_cache_is_read_without_error() {
        let tree = Tree::new("old");
        tree.process(
            100,
            "blender",
            &["PUEUE_GROUP=heavy", "PUEUE_WORKER_ID=1"],
            7,
        );
        tree.gpu(100, 5, 1, 100, 0, 2048 * 1024);
        tree.in_slice("pueued.service", &[100]);
        let t = 8_000_000u128;
        fs::write(
            &tree.cache,
            format!("{t}\n100\t7\t{t}\t34\t2097152\t1:100\theavy\t1\t-\t-\tblender\n"),
        )
        .unwrap();
        let rows = outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 100);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pct, Some(34));
        assert_eq!(rows[0].comm, "blender");
        assert_eq!(
            rows[0].lane.as_ref().map(|lane| lane.group.as_str()),
            Some("heavy")
        );
        let _ = outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 1_000);
        fs::write(&tree.cache, "nope\n").unwrap();
        assert!(outside_in(&tree.proc_root, &tree.slice, &tree.cache, t + 2_000).is_empty());
    }

    fn lane_of_job(tree: &Tree, jobs: &[(u32, u64)], pid: u32) -> Option<Lane> {
        let queue = tree.queue();
        fs::create_dir_all(&queue).unwrap();
        lanes_in(&tree.proc_root, &queue, jobs, 100)
            .remove(&pid)
            .flatten()
    }

    fn lanes_file(tree: &Tree) -> String {
        fs::read_to_string(tree.queue().join("lanes")).unwrap_or_default()
    }

    #[test]
    fn a_jobs_lane_is_read_once_and_dropped_when_the_job_leaves_or_restarts() {
        let tree = Tree::new("lanes");
        tree.boot(1_700_000_000);
        tree.process(50, "pueued", &[], 1);
        tree.parent(50, 1);
        tree.process(60, "sh", &[], 12_345);
        tree.parent(60, 50);
        tree.cwd(60, "/work/job-a");
        tree.process(
            70,
            "pgpu",
            &["PUEUE_GROUP=gpu", "PUEUE_WORKER_ID=1"],
            12_400,
        );
        tree.parent(70, 60);
        let first = lane_of_job(&tree, &[(70, 12_400)], 70).unwrap();
        assert_eq!(first.group, "gpu");
        assert_eq!(first.path.as_deref(), Some("/work/job-a"));
        tree.cwd(60, "/work/other");
        fs::write(
            tree.proc_root.join("70").join("environ"),
            b"PUEUE_GROUP=heavy\0PUEUE_WORKER_ID=0\0",
        )
        .unwrap();
        let second = lane_of_job(&tree, &[(70, 12_400)], 70).unwrap();
        assert_eq!(second.group, "gpu");
        assert_eq!(second.path.as_deref(), Some("/work/job-a"));
        assert!(lane_of_job(&tree, &[], 70).is_none());
        let body = lanes_file(&tree);
        assert!(!body.contains("70\t"), "dropped with its job: {body}");
        let third = lane_of_job(&tree, &[(70, 12_400)], 70).unwrap();
        assert_eq!(third.group, "heavy");
        assert_eq!(third.path.as_deref(), Some("/work/other"));
        fs::write(
            tree.proc_root.join("70").join("environ"),
            b"PUEUE_GROUP=render\0PUEUE_WORKER_ID=2\0",
        )
        .unwrap();
        let fourth = lane_of_job(&tree, &[(70, 50)], 70).unwrap();
        assert_eq!(fourth.group, "render");
        assert_eq!(fourth.worker, Some(2));
        let body = lanes_file(&tree);
        assert!(body.contains("70\t50\t"), "the new start is cached: {body}");
        assert!(!body.contains("70\t12400\t"));
    }

    #[test]
    fn a_lane_with_no_pueued_is_cached_with_null_path_and_started() {
        let tree = Tree::new("lanenull");
        tree.boot(1_700_000_000);
        tree.process(70, "pgpu", &["PUEUE_GROUP=gpu", "PUEUE_WORKER_ID=0"], 200);
        tree.parent(70, 1);
        let first = lane_of_job(&tree, &[(70, 200)], 70).unwrap();
        assert_eq!((first.path, first.started), (None, None));
        tree.process(50, "pueued", &[], 1);
        tree.parent(50, 1);
        tree.process(60, "sh", &[], 100);
        tree.parent(60, 50);
        tree.cwd(60, "/work");
        tree.parent(70, 60);
        let second = lane_of_job(&tree, &[(70, 200)], 70).unwrap();
        assert_eq!(
            (second.path.as_deref(), second.started),
            (None, None),
            "the null path is served from the cache"
        );
        assert!(lane_of_job(&tree, &[], 70).is_none());
        let third = lane_of_job(&tree, &[(70, 200)], 70).unwrap();
        assert_eq!(third.path.as_deref(), Some("/work"));
        assert!(third.started.is_some());
    }

    #[test]
    fn lanes_write_nothing_without_a_queue_folder() {
        let tree = Tree::new("lanenone");
        tree.process(70, "pgpu", &["PUEUE_GROUP=gpu"], 200);
        tree.parent(70, 1);
        let lanes = lanes_in(&tree.proc_root, &tree.queue(), &[(70, 200)], 100);
        assert_eq!(lanes[&70].as_ref().map(|l| l.group.as_str()), Some("gpu"));
        assert!(!tree.queue().exists());
    }
}
