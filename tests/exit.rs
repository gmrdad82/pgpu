mod common;

use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::*;

fn trapping(name: &str, sig: &str, turns: bool) -> String {
    format!(
        "echo $$ > \"$D/{name}.pid\"\ntrap 'echo trapped > \"$D/{name}.trapped\"; sleep 0.3; echo left > \"$D/{name}.left\"; exit 0' {sig}\n{}",
        ticker(name, turns)
    )
}

fn run_without_cores(sb: &mut Sandbox, args: &[&str], name: &str, body: &str) -> u32 {
    let script = sb.path(&format!("jobs/{name}.sh"));
    fs::write(&script, body).unwrap();
    let out = File::create(sb.path(&format!("{name}.out"))).unwrap();
    let err = File::create(sb.path(&format!("{name}.err"))).unwrap();
    let mut cmd = sb.pgpu();
    cmd.arg("run")
        .args(args)
        .arg("--")
        .arg("sh")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            let one = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &one) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    let pid = child.id();
    sb.kids.push(child);
    pid
}

#[test]
fn a_job_blocked_in_turn_runs_its_term_trap_at_once_and_its_wrapper_sees_it_exit() {
    let mut sb = Sandbox::new("exit-term-turn");
    let a = sb.run(
        &["--class", "clip", "--as", "alpha"],
        "a",
        &trapping("a", "TERM", true),
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
    let job = job_pid(&sb, "a");
    let turn = sb.path(&format!("run/pito-gpu-queue/turns/{a}"));
    assert_eq!(fs::read_to_string(&turn).unwrap_or_default().trim(), "wait");
    let sent = Instant::now();
    signal(a, libc::SIGTERM);
    until("the job runs its trap", 5, || sb.path("a.trapped").exists());
    assert!(
        sent.elapsed() < Duration::from_millis(500),
        "the trap ran {:?} after the signal",
        sent.elapsed()
    );
    let status = sb.wait_exit(a, 6);
    assert_eq!(status.code(), Some(143));
    assert!(
        sb.path("a.left").exists(),
        "the wrapper left before its job finished its trap"
    );
    assert!(gone(job));
    assert!(ticket_files(&sb, a).is_empty());
    assert!(!turn.exists());
    assert_eq!(sb.job_state(a), None);
    assert_eq!(sb.job_state(b).as_deref(), Some("running"));
    fs::write(sb.path("b.finish"), "").unwrap();
    assert!(sb.wait_exit(b, 10).success());
}

#[test]
fn int_and_hup_reach_the_job_and_the_wrapper_exits_128_plus_the_signal() {
    let mut sb = Sandbox::new("exit-int-hup");
    for (sig, word, name) in [(libc::SIGINT, "INT", "int"), (libc::SIGHUP, "HUP", "hup")] {
        let wrapper = sb.run(&["--as", name], name, &trapping(name, word, false));
        let ticks = sb.path(&format!("{name}.ticks"));
        until(&format!("{name} ticks"), 10, || size(&ticks) > 0);
        signal(wrapper, sig);
        let status = sb.wait_exit(wrapper, 6);
        assert_eq!(status.code(), Some(128 + sig), "{name}");
        assert!(sb.path(&format!("{name}.trapped")).exists(), "{name}");
        assert!(sb.path(&format!("{name}.left")).exists(), "{name}");
        assert!(ticket_files(&sb, wrapper).is_empty(), "{name}");
        let err = fs::read_to_string(sb.path(&format!("{name}.err"))).unwrap();
        assert!(!err.contains("died by"), "{name}: {err}");
    }
}

#[test]
fn a_command_that_cannot_start_exits_127_and_leaves_no_ticket_holder_or_cell() {
    let mut sb = Sandbox::new("exit-127");
    let missing = sb.path("bin/no-such-command");
    let wrapper = sb.run_command(&["--as", "nope"], "nope", &[&missing.to_string_lossy()]);
    assert_eq!(sb.wait_exit(wrapper, 10).code(), Some(127));
    let err = fs::read_to_string(sb.path("nope.err")).unwrap();
    assert!(err.starts_with("pgpu: cannot run "), "{err}");
    assert!(ticket_files(&sb, wrapper).is_empty());
    assert!(!sb.path("run/pito-gpu-queue/running").exists());
    assert!(
        !sb.path(&format!("run/pito-gpu-queue/stopped/{wrapper}"))
            .exists()
    );
    if let Some(own) = own_cgroup() {
        assert!(!own.join(format!("pgpu-{wrapper}")).exists());
        assert!(
            !own.parent()
                .unwrap()
                .join(format!("pgpu-{wrapper}"))
                .exists()
        );
    }
    assert_eq!(sb.queued(), 0);
}

#[test]
fn a_nested_run_starts_at_once_inside_the_outer_job_and_its_turns_are_the_outer_jobs() {
    let mut sb = Sandbox::new("exit-nested");
    let inner = sb.path("jobs/inner.sh");
    fs::write(
        &inner,
        format!(
            "echo \"$GPU_QUEUE $GPU_QUEUE_PID $GPU_QUEUE_TURN\" > \"$D/inner.env\"\n{}",
            ticker("inner", true)
        ),
    )
    .unwrap();
    let outer = sb.run(
        &["--class", "clip", "--as", "outer"],
        "outer",
        &format!(
            "\"$PGPU\" run --as inner -- sh \"{}\"\n",
            inner.to_string_lossy()
        ),
    );
    let ticks = sb.path("inner.ticks");
    until("the nested job runs", 10, || size(&ticks) > 0);
    let jobs = sb.status().field("jobs").items().to_vec();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].field("pid").num() as u32, outer);
    let env = fs::read_to_string(sb.path("inner.env")).unwrap();
    let turn = sb.path(&format!("run/pito-gpu-queue/turns/{outer}"));
    assert_eq!(
        env.trim(),
        format!("1 {outer} {}", turn.to_string_lossy()),
        "{env}"
    );
    assert!(
        fs::read_dir(sb.path("run/pito-gpu-queue/tickets"))
            .unwrap()
            .flatten()
            .count()
            == 1
    );
    let b = sb.run(
        &["--class", "clip", "--as", "beta"],
        "b",
        &ticker("b", false),
    );
    until("the nested turn yields the outer job to b", 20, || {
        sb.job_state(outer).as_deref() == Some("yielded")
            && sb.job_state(b).as_deref() == Some("running")
    });
    fs::write(sb.path("b.finish"), "").unwrap();
    assert!(sb.wait_exit(b, 10).success());
    until("the outer job holds the gpu again", 10, || {
        sb.job_state(outer).as_deref() == Some("running")
    });
    fs::write(sb.path("inner.finish"), "").unwrap();
    assert!(sb.wait_exit(outer, 10).success());
    assert!(sb.path("inner.done").exists());
}

#[test]
fn a_job_killed_by_sigsegv_says_so_and_a_cancelled_job_does_not() {
    let mut sb = Sandbox::new("exit-segv");
    let segv = run_without_cores(&mut sb, &["--as", "segv"], "segv", "kill -SEGV $$\n");
    assert_eq!(sb.wait_exit(segv, 15).code(), Some(139));
    assert_eq!(
        lines(&sb.path("segv.err")),
        vec!["pgpu: job died by SIGSEGV (signal 11)".to_string()]
    );
    let cancelled = sb.run(
        &["--as", "cancelled"],
        "cancelled",
        &ticker("cancelled", false),
    );
    let ticks = sb.path("cancelled.ticks");
    until("the job ticks", 10, || size(&ticks) > 0);
    signal(cancelled, libc::SIGTERM);
    assert_eq!(sb.wait_exit(cancelled, 6).code(), Some(143));
    let err = fs::read_to_string(sb.path("cancelled.err")).unwrap();
    assert!(!err.contains("died by"), "{err}");
}

#[test]
fn an_interrupted_or_finished_job_leaves_no_process_behind_even_one_that_ignores_signals() {
    let mut sb = Sandbox::new("exit-leftover");
    let deaf = |name: &str, end: &str| {
        format!("( trap '' INT TERM HUP; exec sleep 60 ) &\necho $! > \"$D/{name}.pid\"\n{end}\n")
    };
    let interrupted = sb.run(
        &["--as", "interrupted"],
        "interrupted",
        &deaf("interrupted", "wait"),
    );
    let left = job_pid(&sb, "interrupted");
    let cell = cell_of(&sb, interrupted);
    signal(interrupted, libc::SIGINT);
    assert_eq!(sb.wait_exit(interrupted, 10).code(), Some(130));
    assert!(
        gone(left),
        "the job's child outlived its interrupted wrapper"
    );
    assert!(!cell.exists(), "the job's cgroup outlived its wrapper");
    let finished = sb.run(
        &["--as", "finished"],
        "finished",
        &deaf("finished", "exit 0"),
    );
    let left = job_pid(&sb, "finished");
    assert!(sb.wait_exit(finished, 10).success());
    assert!(gone(left), "the job's child outlived its finished wrapper");
}
