mod common;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;

fn stamps(sb: &Sandbox, name: &str) -> Vec<u128> {
    lines(&sb.path(name))
        .iter()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

#[test]
fn a_job_that_turns_every_2_s_hands_over_to_a_waiter_with_no_idle_rest_first() {
    let mut sb = Sandbox::new("turn-no-rest");
    sb.config("share", "25\n");
    let body = "while [ ! -e \"$D/slow.finish\" ]; do\nsleep 2\ndate +%s%3N >> \"$D/slow.calls\"\n\"$PGPU\" turn --work-ms 2000 --longest-ms 4\ndate +%s%3N >> \"$D/slow.back\"\ndone\n";
    let slow = sb.run(&["--class", "clip", "--as", "slow"], "slow", body);
    until("the slow job runs", 10, || {
        sb.job_state(slow).as_deref() == Some("running")
    });
    let waiter = sb.run(
        &["--class", "clip", "--as", "waiter"],
        "waiter",
        "date +%s%3N > \"$D/waiter.started\"\n",
    );
    assert!(sb.wait_exit(waiter, 30).success(), "the waiter never ran");
    let started = stamps(&sb, "waiter.started")[0];
    let called = stamps(&sb, "slow.calls")
        .into_iter()
        .filter(|at| *at <= started)
        .max()
        .expect("the waiter started before the slow job turned");
    let gap = started - called;
    assert!(
        gap < 1500,
        "the waiter started {gap} ms after the turn that let it in"
    );
    until("the slow job gets the gpu back", 10, || {
        stamps(&sb, "slow.back").iter().any(|at| *at >= started)
    });
    fs::write(sb.path("slow.finish"), "").unwrap();
    assert!(sb.wait_exit(slow, 15).success());
}

#[test]
fn a_turn_whose_wrapper_is_dead_or_unknown_returns_at_once_and_leaves_no_file() {
    let sb = Sandbox::new("turn-orphan");
    sb.config("share", "25\n");
    let mut stranger = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let turns = sb.path("run/pito-gpu-queue/turns");
    for pid in [stranger.id(), 4_194_305] {
        let asked = Instant::now();
        let status = sb
            .pgpu()
            .args(["turn", "--work-ms", "3000", "--longest-ms", "4"])
            .env("GPU_QUEUE_TURN", turns.join(pid.to_string()))
            .env("GPU_QUEUE_PID", pid.to_string())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(
            asked.elapsed() < Duration::from_millis(500),
            "turn for {pid} took {:?}",
            asked.elapsed()
        );
        let left: Vec<String> = fs::read_dir(&turns)
            .map(|dir| {
                dir.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(&pid.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        assert!(left.is_empty(), "turn for {pid} left {left:?}");
    }
    let _ = stranger.kill();
    let _ = stranger.wait();
}
