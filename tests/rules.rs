mod common;

use std::fs;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::*;

fn stamped(name: &str, turns: bool) -> String {
    format!("date +%s%3N > \"$D/{name}.begin\"\n{}", ticker(name, turns))
}

fn begin(sb: &Sandbox, name: &str) -> u128 {
    let path = sb.path(&format!("{name}.begin"));
    let mut at = None;
    until(&format!("{name} writes its start"), 15, || {
        at = fs::read_to_string(&path)
            .ok()
            .and_then(|body| body.trim().parse().ok());
        at.is_some()
    });
    at.unwrap()
}

fn finish(sb: &mut Sandbox, wrapper: u32, name: &str) {
    fs::write(sb.path(&format!("{name}.finish")), "").unwrap();
    assert!(sb.wait_exit(wrapper, 15).success(), "{name} did not finish");
}

#[test]
fn two_long_jobs_of_one_name_and_class_swap_after_the_3_s_slice() {
    let mut sb = Sandbox::new("rules-slice");
    let args = ["--class", "clip", "--as", "same"];
    let a = sb.run(&args, "a", &stamped("a", true));
    begin(&sb, "a");
    let b = sb.run(&args, "b", &stamped("b", true));
    let swapped = begin(&sb, "b") - begin(&sb, "a");
    assert!(
        (2800..=4500).contains(&swapped),
        "the second job got the gpu {swapped} ms after the first, not after its 3 s slice"
    );
    finish(&mut sb, b, "b");
    finish(&mut sb, a, "a");
}

#[test]
fn an_interactive_job_gets_in_at_the_next_turn_even_inside_the_holders_slice() {
    let mut sb = Sandbox::new("rules-slice-class");
    let a = sb.run(
        &["--class", "clip", "--as", "alpha"],
        "a",
        &stamped("a", true),
    );
    let started = begin(&sb, "a");
    sleep(Duration::from_millis(500));
    let asked = Instant::now();
    let hi = sb.run(
        &["--class", "interactive", "--as", "urgent"],
        "hi",
        "date +%s%3N > \"$D/hi.begin\"\necho done > \"$D/hi.done\"\n",
    );
    let got = begin(&sb, "hi");
    assert!(
        asked.elapsed() < Duration::from_millis(2500),
        "the interactive job took {:?} to get in",
        asked.elapsed()
    );
    assert!(
        got - started < 2500,
        "the interactive job got in {} ms into the slice, so it waited it out",
        got - started
    );
    assert!(sb.wait_exit(hi, 15).success());
    let ticks = sb.path("a.ticks");
    let at = size(&ticks);
    until("the holder gets the gpu back", 15, || size(&ticks) > at);
    finish(&mut sb, a, "a");
}

fn day_of(secs: u128) -> String {
    let out = Command::new("date")
        .args(["-u", "-d", &format!("@{secs}"), "+%F"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn stats_gives_each_day_its_held_active_busy_holds_and_wait_percentiles() {
    let sb = Sandbox::new("rules-stats");
    let today = now_ms_of_test() as u128 / 86_400_000;
    let first = (today - 3) * 86_400_000 + 3_600_000;
    let second = (today - 1) * 86_400_000 + 3_600_000;
    let old = (today - 20) * 86_400_000;
    let hold = |at: u128, from: u128, to: u128, who: &str, class: u8, wait: u128| {
        format!(
            "{}\t{}\t{who}\t{class}\t{wait}\t{}\tdone\n",
            at + from * 1000,
            at + to * 1000,
            4000 + from
        )
    };
    let log = [
        hold(first, 0, 60, "alpha", 1, 100),
        hold(first, 70, 130, "beta", 1, 400),
        hold(first, 140, 200, "alpha", 1, 200),
        hold(first, 210, 230, "alpha", 2, 300),
        "not a hold\n".to_string(),
        hold(second, 0, 30, "beta", 1, 50),
        hold(old, 0, 60, "gamma", 1, 9000),
    ]
    .concat();
    fs::create_dir_all(sb.path("state/pito-gpu-queue")).unwrap();
    fs::write(sb.path("state/pito-gpu-queue/log.tsv"), log).unwrap();
    let run = |args: &[&str]| {
        let out = sb.pgpu().env("TZ", "UTC").arg("stats").args(args).output();
        let out = out.unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let json = Json::parse(run(&["--json"]).trim());
    assert_eq!(json.field("v").num(), 1.0);
    let days = json.field("days").items();
    assert_eq!(days.len(), 2, "{days:?}");
    let a = &days[0];
    assert_eq!(a.field("day").text(), day_of(first / 1000));
    assert_eq!(a.field("held_ms").num(), 200_000.0);
    assert_eq!(a.field("active_ms").num(), 230_000.0);
    assert_eq!(a.field("busy").num(), 86.0);
    assert_eq!(a.field("holds").num(), 4.0);
    let waits = a.field("wait_ms");
    assert_eq!(waits.field("p50").num(), 300.0);
    assert_eq!(waits.field("p90").num(), 400.0);
    assert_eq!(waits.field("max").num(), 400.0);
    let sessions = a.field("sessions").items();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].field("as").text(), "alpha");
    assert_eq!(sessions[0].field("held_ms").num(), 140_000.0);
    assert_eq!(sessions[0].field("share").num(), 70.0);
    assert_eq!(sessions[1].field("as").text(), "beta");
    assert_eq!(sessions[1].field("share").num(), 30.0);
    let b = &days[1];
    assert_eq!(b.field("day").text(), day_of(second / 1000));
    assert_eq!(b.field("held_ms").num(), 30_000.0);
    assert_eq!(b.field("active_ms").num(), 30_000.0);
    assert_eq!(b.field("busy").num(), 100.0);
    assert_eq!(b.field("holds").num(), 1.0);
    assert_eq!(b.field("wait_ms").field("p50").num(), 50.0);
    assert_eq!(b.field("wait_ms").field("max").num(), 50.0);
    let text = run(&[]);
    let row: Vec<&str> = text
        .lines()
        .find(|line| line.starts_with(&day_of(first / 1000)))
        .unwrap_or_else(|| panic!("no row for the first day: {text}"))
        .split_whitespace()
        .collect();
    assert_eq!(
        row[1..],
        [
            "3m20s", "3m50s", "86%", "4", "0.3s", "/", "0.4s", "/", "0.4s", "alpha", "70%,",
            "beta", "30%"
        ],
        "{text}"
    );
    assert!(!text.contains("gamma"), "{text}");
}

#[test]
fn pause_leaves_a_lent_job_running_on_the_cpu_and_starts_no_waiting_job() {
    let mut sb = Sandbox::new("rules-pause-lent");
    let (lender, waiter) = lend_to_a_busy_waiter(&mut sb, "lender", &ticker("lender", false));
    let out = sb.pgpu().arg("pause").output().unwrap();
    assert!(out.status.success());
    let late = sb.run(
        &["--class", "clip", "--as", "late"],
        "late",
        &marker("late"),
    );
    until("the late job is queued", 10, || {
        sb.job_state(late).as_deref() == Some("waiting")
    });
    fs::write(sb.path("busy.finish"), "").unwrap();
    assert!(sb.wait_exit(waiter, 15).success());
    let ticks = sb.path("lender.ticks");
    let before = size(&ticks);
    holds("a late job started while the queue was paused", 3.0, || {
        !sb.path("late.started").exists()
    });
    assert!(size(&ticks) > before, "the lent job stopped running");
    assert_eq!(sb.job_state(lender).as_deref(), Some("lent"));
    assert_eq!(sb.job_state(late).as_deref(), Some("waiting"));
    let out = sb.pgpu().arg("resume").output().unwrap();
    assert!(out.status.success());
    until("the late job starts and ends once resumed", 15, || {
        sb.path("late.done").exists()
    });
    assert!(sb.wait_exit(late, 10).success());
    finish(&mut sb, lender, "lender");
}
