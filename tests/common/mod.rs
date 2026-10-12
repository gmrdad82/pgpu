#![allow(dead_code)]

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub const PGPU: &str = env!("CARGO_BIN_EXE_pgpu");

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    pub fn blank(&mut self) {
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    pub fn take(&mut self, want: u8) {
        self.blank();
        assert_eq!(
            self.bytes.get(self.at),
            Some(&want),
            "expected {:?} at {}",
            want as char,
            self.at
        );
        self.at += 1;
    }

    pub fn peek(&mut self) -> u8 {
        self.blank();
        *self.bytes.get(self.at).expect("json ended early")
    }

    pub fn word(&mut self, text: &str, value: Json) -> Json {
        assert!(
            self.bytes[self.at..].starts_with(text.as_bytes()),
            "bad json word at {}",
            self.at
        );
        self.at += text.len();
        value
    }

    pub fn value(&mut self) -> Json {
        match self.peek() {
            b'{' => {
                self.take(b'{');
                let mut fields = Vec::new();
                if self.peek() == b'}' {
                    self.take(b'}');
                    return Json::Obj(fields);
                }
                loop {
                    let key = self.string();
                    self.take(b':');
                    fields.push((key, self.value()));
                    if self.peek() == b',' {
                        self.take(b',');
                    } else {
                        self.take(b'}');
                        return Json::Obj(fields);
                    }
                }
            }
            b'[' => {
                self.take(b'[');
                let mut items = Vec::new();
                if self.peek() == b']' {
                    self.take(b']');
                    return Json::Arr(items);
                }
                loop {
                    items.push(self.value());
                    if self.peek() == b',' {
                        self.take(b',');
                    } else {
                        self.take(b']');
                        return Json::Arr(items);
                    }
                }
            }
            b'"' => Json::Str(self.string()),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'n' => self.word("null", Json::Null),
            _ => {
                let from = self.at;
                while self.at < self.bytes.len()
                    && matches!(
                        self.bytes[self.at],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.bytes[from..self.at]).unwrap();
                Json::Num(
                    text.parse()
                        .unwrap_or_else(|_| panic!("bad number {text:?}")),
                )
            }
        }
    }

    pub fn string(&mut self) -> String {
        self.take(b'"');
        let mut out = Vec::new();
        loop {
            let byte = *self.bytes.get(self.at).expect("string ended early");
            self.at += 1;
            match byte {
                b'"' => return String::from_utf8(out).expect("json string is utf-8"),
                b'\\' => {
                    let esc = self.bytes[self.at];
                    self.at += 1;
                    match esc {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex =
                                std::str::from_utf8(&self.bytes[self.at..self.at + 4]).unwrap();
                            self.at += 4;
                            let code = u32::from_str_radix(hex, 16).unwrap();
                            let ch = char::from_u32(code).unwrap_or('?');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
    }
}

impl Json {
    pub fn parse(text: &str) -> Json {
        let mut reader = Reader {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = reader.value();
        reader.blank();
        assert_eq!(reader.at, text.len(), "trailing text after the json");
        value
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn field(&self, key: &str) -> &Json {
        self.get(key)
            .unwrap_or_else(|| panic!("missing field {key} in {self:?}"))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn text(&self) -> &str {
        match self {
            Json::Str(text) => text,
            other => panic!("not a string: {other:?}"),
        }
    }

    pub fn num(&self) -> f64 {
        match self {
            Json::Num(n) => *n,
            other => panic!("not a number: {other:?}"),
        }
    }

    pub fn flag(&self) -> bool {
        match self {
            Json::Bool(b) => *b,
            other => panic!("not a bool: {other:?}"),
        }
    }

    pub fn items(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            other => panic!("not an array: {other:?}"),
        }
    }
}

pub fn until(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(Instant::now() < end, "timed out after {secs} s: {what}");
        sleep(Duration::from_millis(50));
    }
}

pub fn holds(what: &str, secs: f64, mut still: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        assert!(still(), "{what}");
        sleep(Duration::from_millis(50));
    }
}

pub fn size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

pub fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

pub fn until_still(what: &str, secs: u64, path: &Path) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut last = size(path);
    let mut since = Instant::now();
    loop {
        sleep(Duration::from_millis(100));
        let now = size(path);
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(1500) && now > 0 {
            return;
        }
        assert!(Instant::now() < end, "timed out after {secs} s: {what}");
    }
}

pub fn stat_of(pid: u32) -> Option<(char, u32)> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &text[text.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    fields.next()?;
    let group = fields.next()?.parse().ok()?;
    Some((state, group))
}

pub fn group_states(group: u32) -> Vec<(u32, char)> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some((state, pgrp)) = stat_of(pid)
            && pgrp == group
        {
            found.push((pid, state));
        }
    }
    found
}

pub fn parked(group: u32) -> usize {
    group_states(group)
        .iter()
        .filter(|(_, state)| *state == 'T')
        .count()
}

pub fn own_cgroup() -> Option<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    Some(Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

pub fn cell_of(sb: &Sandbox, wrapper: u32) -> PathBuf {
    let entry = sb.path(&format!("run/pito-gpu-queue/stopped/{wrapper}"));
    let first = lines(&entry).into_iter().next().unwrap_or_default();
    let path = PathBuf::from(
        first
            .strip_prefix("cgroup ")
            .unwrap_or_else(|| panic!("no cgroup in {}: {first:?}", entry.display())),
    );
    assert!(
        path.file_name()
            .is_some_and(|name| name.to_string_lossy() == format!("pgpu-{wrapper}"))
    );
    path
}

pub fn frozen(cell: &Path) -> bool {
    lines(&cell.join("cgroup.events"))
        .iter()
        .any(|line| line == "frozen 1")
}

pub fn cell_threads(cell: &Path) -> Vec<(u32, char)> {
    lines(&cell.join("cgroup.threads"))
        .iter()
        .filter_map(|tid| tid.parse::<u32>().ok())
        .filter_map(|tid| stat_of(tid).map(|(state, _)| (tid, state)))
        .collect()
}

pub struct Jail {
    pub path: PathBuf,
}

impl Jail {
    pub fn new(name: &str) -> Option<Self> {
        let path = own_cgroup()?.join(format!("pgpu-e2e-{}-{name}", std::process::id()));
        fs::create_dir(&path).ok()?;
        let jail = Self { path };
        fs::set_permissions(&jail.path, fs::Permissions::from_mode(0o555)).ok()?;
        Some(jail)
    }

    pub fn procs(&self) -> PathBuf {
        self.path.join("cgroup.procs")
    }
}

impl Drop for Jail {
    fn drop(&mut self) {
        let out = self.path.parent().unwrap().join("cgroup.procs");
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755));
        for _ in 0..200 {
            for pid in lines(&self.procs()) {
                let _ = fs::OpenOptions::new()
                    .write(true)
                    .open(&out)
                    .and_then(|mut file| file.write_all(pid.as_bytes()));
            }
            if fs::remove_dir(&self.path).is_ok() {
                return;
            }
            sleep(Duration::from_millis(10));
        }
    }
}

pub fn signal(pid: u32, sig: i32) {
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

pub fn ticker(name: &str, turns: bool) -> String {
    let turn = if turns { "\"$PGPU\" turn\n" } else { "" };
    format!(
        "while [ ! -e \"$D/{name}.finish\" ]; do\necho x >> \"$D/{name}.ticks\"\n{turn}sleep 0.05\ndone\necho done > \"$D/{name}.done\"\n"
    )
}

pub fn marker(name: &str) -> String {
    format!("echo started > \"$D/{name}.started\"\necho done > \"$D/{name}.done\"\n")
}

pub fn test_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tmp")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Sandbox {
    pub dir: PathBuf,
    pub kids: Vec<Child>,
    pub env: Vec<(String, String)>,
}

impl Sandbox {
    pub fn new(test: &str) -> Self {
        let dir = test_dir(&format!("e2e-{test}"));
        for sub in ["run", "state", "config/pgpu", "bin", "home", "jobs", "fake"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let sandbox = Self {
            dir,
            kids: Vec::new(),
            env: Vec::new(),
        };
        sandbox.config("lend", "off\n");
        sandbox.fake("card_busy", "100\n");
        sandbox
    }

    pub fn fake(&self, name: &str, value: &str) {
        fs::write(self.dir.join("fake").join(name), value).unwrap();
    }

    pub fn config(&self, name: &str, value: &str) {
        self.unset(name);
        let path = self.dir.join("config/pgpu/config");
        let mut body = fs::read_to_string(&path).unwrap_or_default();
        body.push_str(&format!("{}={}\n", name.to_uppercase(), value.trim()));
        fs::write(path, body).unwrap();
    }

    pub fn unset(&self, name: &str) {
        let path = self.dir.join("config/pgpu/config");
        let key = format!("{}=", name.to_uppercase());
        let body: String = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.starts_with(&key))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(path, body).unwrap();
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn pgpu(&self) -> Command {
        let mut cmd = Command::new(PGPU);
        for var in [
            "GPU_QUEUE",
            "GPU_QUEUE_TURN",
            "GPU_QUEUE_PID",
            "AMD_PRIORITY",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "PUEUE_GROUP",
            "PUEUE_WORKER_ID",
        ] {
            cmd.env_remove(var);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd.env("XDG_RUNTIME_DIR", self.dir.join("run"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("HOME", self.dir.join("home"))
            .env("PGPU_FAKE", self.dir.join("fake"))
            .env("D", &self.dir)
            .env("PGPU", PGPU)
            .current_dir(&self.dir);
        cmd
    }

    pub fn run(&mut self, args: &[&str], name: &str, body: &str) -> u32 {
        let script = self.dir.join("jobs").join(format!("{name}.sh"));
        fs::write(&script, body).unwrap();
        let script = script.to_string_lossy().into_owned();
        self.run_command(args, name, &["sh", &script])
    }

    pub fn run_in(&mut self, procs: &Path, args: &[&str], name: &str, body: &str) -> u32 {
        let door = fs::OpenOptions::new().write(true).open(procs).unwrap();
        let script = self.dir.join("jobs").join(format!("{name}.sh"));
        fs::write(&script, body).unwrap();
        let script = script.to_string_lossy().into_owned();
        self.spawn(args, name, &["sh", &script], Some(&door))
    }

    pub fn run_command(&mut self, args: &[&str], name: &str, command: &[&str]) -> u32 {
        self.spawn(args, name, command, None)
    }

    pub fn spawn(
        &mut self,
        args: &[&str],
        name: &str,
        command: &[&str],
        door: Option<&File>,
    ) -> u32 {
        let out = File::create(self.path(&format!("{name}.out"))).unwrap();
        let err = File::create(self.path(&format!("{name}.err"))).unwrap();
        let mut cmd = self.pgpu();
        cmd.arg("run")
            .args(args)
            .arg("--")
            .args(command)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        let fd = door.map(|door| door.as_raw_fd());
        unsafe {
            cmd.pre_exec(move || {
                libc::setsid();
                if let Some(fd) = fd
                    && libc::write(fd, b"0".as_ptr().cast(), 1) != 1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let pid = child.id();
        self.kids.push(child);
        pid
    }

    pub fn wait_exit(&mut self, pid: u32, secs: u64) -> ExitStatus {
        let kid = self.kids.iter_mut().find(|k| k.id() == pid).unwrap();
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(status) = kid.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < end,
                "wrapper {pid} still running after {secs} s"
            );
            sleep(Duration::from_millis(20));
        }
    }

    pub fn status(&self) -> Json {
        let out = self.pgpu().args(["status", "--json"]).output().unwrap();
        assert!(out.status.success(), "status --json failed");
        Json::parse(String::from_utf8(out.stdout).unwrap().trim())
    }

    pub fn status_text(&self) -> String {
        let out = self.pgpu().arg("status").output().unwrap();
        assert!(out.status.success(), "status failed");
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn job_state(&self, pid: u32) -> Option<String> {
        self.status()
            .field("jobs")
            .items()
            .iter()
            .find(|job| job.field("pid").num() as u32 == pid)
            .map(|job| job.field("state").text().to_string())
    }

    pub fn waits_for(&self, pid: u32) -> Option<String> {
        self.status()
            .field("jobs")
            .items()
            .iter()
            .find(|job| job.field("pid").num() as u32 == pid)
            .and_then(|job| match job.field("waits_for") {
                Json::Str(text) => Some(text.clone()),
                _ => None,
            })
    }

    pub fn queued(&self) -> usize {
        self.status().field("jobs").items().len()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for kid in &self.kids {
            signal(kid.id(), libc::SIGTERM);
        }
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end
            && self
                .kids
                .iter_mut()
                .any(|kid| !matches!(kid.try_wait(), Ok(Some(_))))
        {
            sleep(Duration::from_millis(20));
        }
        for kid in self.kids.iter_mut() {
            unsafe {
                libc::kill(-(kid.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = kid.kill();
            let _ = kid.wait();
        }
    }
}

pub fn queue_and_wait(sb: &mut Sandbox, args: &[&str], name: &str, body: &str) -> u32 {
    let before = sb.queued();
    let pid = sb.run(args, name, body);
    until(&format!("{name} is queued"), 10, || sb.queued() > before);
    pid
}

pub fn lend_then_take_back(sb: &mut Sandbox, name: &str, wrapper: u32) -> u32 {
    sb.fake("gpu.other", "90\n");
    let other = sb.run(&["--as", "other"], "other", &ticker("other", false));
    until("the job lends the gpu", 20, || {
        sb.job_state(wrapper).as_deref() == Some("lent")
    });
    until("the other job runs", 10, || {
        sb.job_state(other).as_deref() == Some("running")
    });
    sb.fake(&format!("gpu.{name}"), "90\n");
    other
}

pub fn park_a_job_by_take_back(sb: &mut Sandbox, name: &str) -> (u32, PathBuf) {
    sb.config("lend", "on\n");
    sb.fake(&format!("gpu.{name}"), "0\n");
    let wrapper = sb.run(&["--as", name], name, &ticker(name, false));
    let ticks = sb.path(&format!("{name}.ticks"));
    until("the job ticks", 10, || size(&ticks) > 0);
    let cell = cell_of(sb, wrapper);
    lend_then_take_back(sb, name, wrapper);
    until_still("the job is parked", 20, &ticks);
    until("the job's cgroup is frozen", 5, || frozen(&cell));
    assert_eq!(
        parked(wrapper),
        0,
        "a frozen job shows stopped processes: {:?}",
        group_states(wrapper)
    );
    (wrapper, cell)
}

impl Json {
    pub fn text_or_null(&self) -> Option<&str> {
        match self {
            Json::Null => None,
            other => Some(other.text()),
        }
    }
}

pub fn null_or_bool(value: &Json) {
    assert!(
        value.is_null() || matches!(value, Json::Bool(_)),
        "{value:?}"
    );
}

pub fn null_or_num(value: &Json) {
    assert!(
        value.is_null() || matches!(value, Json::Num(_)),
        "{value:?}"
    );
}

pub fn now_ms_of_test() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64
}

pub fn with_pid(name: &str, turns: bool) -> String {
    format!("echo $$ > \"$D/{name}.pid\"\n{}", ticker(name, turns))
}

pub fn job_pid(sb: &Sandbox, name: &str) -> u32 {
    until(&format!("{name} writes its pid"), 10, || {
        fs::read_to_string(sb.path(&format!("{name}.pid")))
            .is_ok_and(|t| t.trim().parse::<u32>().is_ok())
    });
    fs::read_to_string(sb.path(&format!("{name}.pid")))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

pub fn gone(pid: u32) -> bool {
    stat_of(pid).is_none_or(|(state, _)| state == 'Z')
}

pub fn ticket_files(sb: &Sandbox, wrapper: u32) -> Vec<String> {
    let suffix = format!("-{wrapper}");
    fs::read_dir(sb.path("run/pito-gpu-queue/tickets"))
        .map(|dir| {
            dir.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(&suffix))
                .collect()
        })
        .unwrap_or_default()
}

pub fn yield_a_to_b(sb: &mut Sandbox) -> (u32, u32) {
    let a = sb.run(
        &["--class", "clip", "--as", "alpha"],
        "a",
        &with_pid("a", true),
    );
    let ticks = sb.path("a.ticks");
    until("a ticks", 10, || size(&ticks) > 0);
    let b = sb.run(
        &["--class", "clip", "--as", "beta"],
        "b",
        &ticker("b", false),
    );
    until("a yields and b runs", 20, || {
        sb.job_state(a).as_deref() == Some("yielded")
            && sb.job_state(b).as_deref() == Some("running")
    });
    (a, b)
}

pub fn desk_mark(sb: &Sandbox, wrapper: u32) -> PathBuf {
    sb.path(&format!("run/pito-gpu-queue/desk/{wrapper}"))
}

pub fn desk_held(sb: &Sandbox, wrapper: u32) -> bool {
    sb.job_state(wrapper).as_deref() == Some("waiting")
        && sb.waits_for(wrapper).as_deref() == Some("desk")
}

pub fn ticks_in(path: &Path, secs: f64) -> u64 {
    let from = size(path);
    sleep(Duration::from_secs_f64(secs));
    (size(path) - from) / 2
}

pub fn you_line(sb: &Sandbox) -> String {
    sb.status_text()
        .lines()
        .find(|line| line.starts_with("you "))
        .unwrap_or_default()
        .to_string()
}

pub fn reporter(name: &str, flags: &str) -> String {
    reporter_every(name, flags, 0.0, 0.05)
}

pub fn reporter_every(name: &str, flags: &str, first: f64, every: f64) -> String {
    let wait = if first > 0.0 {
        format!("sleep {first}\n")
    } else {
        String::new()
    };
    format!(
        "{wait}while [ ! -e \"$D/{name}.finish\" ]; do\necho x >> \"$D/{name}.ticks\"\n\"$PGPU\" turn {flags}\nsleep {every}\ndone\necho done > \"$D/{name}.done\"\n"
    )
}

pub fn job_json(sb: &Sandbox, pid: u32) -> Option<Json> {
    sb.status()
        .field("jobs")
        .items()
        .iter()
        .find(|job| job.field("pid").num() as u32 == pid)
        .cloned()
}

pub fn pace_lines(sb: &Sandbox, who: &str) -> Vec<Vec<String>> {
    lines(&sb.path("state/pito-gpu-queue/pace.tsv"))
        .iter()
        .map(|line| line.split('\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|f| f.get(1).is_some_and(|w| w == who))
        .collect()
}

pub fn lent_mark(sb: &Sandbox, wrapper: u32) -> PathBuf {
    sb.path(&format!("run/pito-gpu-queue/lent/{wrapper}"))
}

pub fn lend_to_a_busy_waiter(sb: &mut Sandbox, name: &str, body: &str) -> (u32, u32) {
    sb.config("lend", "on\n");
    sb.fake(&format!("gpu.{name}"), "0\n");
    sb.fake("gpu.busy", "90\n");
    let launched = Instant::now();
    let lender = sb.run(&["--class", "clip", "--as", name], name, body);
    let ticks = sb.path(&format!("{name}.ticks"));
    until("the lender ticks", 10, || size(&ticks) > 0);
    let waiter = sb.run(
        &["--class", "clip", "--as", "busy"],
        "busy",
        &ticker("busy", false),
    );
    until("the waiter waits", 10, || {
        sb.job_state(waiter).as_deref() == Some("waiting")
    });
    until("the lender lends the gpu to the waiter", 25, || {
        sb.job_state(lender).as_deref() == Some("lent")
            && sb.job_state(waiter).as_deref() == Some("running")
    });
    assert!(
        launched.elapsed() >= Duration::from_secs(3),
        "lent {:?} after the lender was launched, before 3 s of looks",
        launched.elapsed()
    );
    assert!(lent_mark(sb, lender).exists());
    (lender, waiter)
}
