mod common;

use std::fs;
use std::time::{Duration, Instant};

use common::*;

fn lanes(sb: &Sandbox, count: u32) {
    let from = sb.path("machine.env");
    fs::write(&from, format!("QUEUE_SLOTS=3\nGPU_SLOTS={count}\n")).unwrap();
    sb.config("gpu_slots", "1");
    sb.config("gpu_slots_from", from.to_str().unwrap());
}

fn why(sb: &Sandbox, pid: u32) -> String {
    sb.status()
        .field("jobs")
        .items()
        .iter()
        .find(|job| job.field("pid").num() as u32 == pid)
        .map(|job| job.field("why").text().to_string())
        .unwrap_or_default()
}

fn running(sb: &Sandbox, pid: u32) -> bool {
    sb.job_state(pid).as_deref() == Some("running")
}

#[test]
fn two_lanes_run_two_jobs_at_once_and_a_third_says_it_waits_its_turn() {
    let mut sb = Sandbox::new("lanes-two");
    lanes(&sb, 2);
    let a = sb.run(
        &["--as", "alpha", "--class", "clip"],
        "a",
        &ticker("a", false),
    );
    let b = sb.run(
        &["--as", "beta", "--class", "clip"],
        "b",
        &ticker("b", false),
    );
    until("both run", 10, || running(&sb, a) && running(&sb, b));
    until("both tick", 10, || {
        size(&sb.path("a.ticks")) > 0 && size(&sb.path("b.ticks")) > 0
    });
    let c = sb.run(
        &["--as", "gamma", "--class", "clip"],
        "c",
        &ticker("c", false),
    );
    until("c waits its turn", 10, || {
        sb.waits_for(c).as_deref() == Some("turn")
    });
    assert!(
        why(&sb, c).contains("all 2 GPU lanes busy"),
        "{}",
        why(&sb, c)
    );
    let text = sb.status_text();
    assert_eq!(
        text.lines().filter(|l| l.starts_with("running")).count(),
        2,
        "{text}"
    );
    assert!(!sb.path("c.ticks").exists());
    fs::write(sb.path("a.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 10).success());
    until("c takes the free lane", 10, || running(&sb, c));
    for job in ["b", "c"] {
        fs::write(sb.path(&format!("{job}.finish")), "").unwrap();
    }
    assert!(sb.wait_exit(b, 10).success());
    assert!(sb.wait_exit(c, 10).success());
}

#[test]
fn two_clips_that_both_turn_keep_both_lanes_and_a_third_takes_turns_on_them() {
    let mut sb = Sandbox::new("lanes-two-turn");
    lanes(&sb, 2);
    let a = sb.run(
        &["--as", "alpha", "--class", "clip"],
        "a",
        &ticker("a", true),
    );
    let b = sb.run(
        &["--as", "beta", "--class", "clip"],
        "b",
        &ticker("b", true),
    );
    until("both run", 10, || running(&sb, a) && running(&sb, b));
    holds("both keep a lane at their turns", 5.0, || {
        running(&sb, a) && running(&sb, b)
    });
    let c = sb.run(
        &["--as", "gamma", "--class", "clip"],
        "c",
        &ticker("c", true),
    );
    let end = Instant::now() + Duration::from_secs(15);
    let mut two = 0;
    let mut looks = 0;
    let mut why_less = Vec::new();
    while Instant::now() < end {
        let jobs = sb.status().field("jobs").items().to_vec();
        let held = jobs
            .iter()
            .filter(|job| job.field("state").text() == "running")
            .count();
        looks += 1;
        if held == 2 {
            two += 1;
        }
        for job in &jobs {
            if job.field("state").text() != "running" && job.field("waits_for").is_null() {
                why_less.push(job.field("as").text().to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(size(&sb.path("c.ticks")) > 0, "c got a turn");
    assert!(
        two * 10 >= looks * 9,
        "two lanes held {two} of {looks} looks"
    );
    assert!(
        why_less.len() * 20 <= looks,
        "waiting with no reason: {why_less:?}"
    );
    for job in ["a", "b", "c"] {
        fs::write(sb.path(&format!("{job}.finish")), "").unwrap();
    }
    for pid in [a, b, c] {
        assert!(sb.wait_exit(pid, 20).success());
    }
}

#[test]
fn a_rerun_alone_needs_a_reason_waits_for_an_empty_queue_by_day_and_at_night_lets_running_jobs_end()
{
    let mut sb = Sandbox::new("lanes-alone");
    lanes(&sb, 2);
    sb.fake("night", "0\n");
    for args in [
        &["run", "--exclusive", "--", "true"][..],
        &["run", "--alone", " ", "--", "true"][..],
    ] {
        let refused = sb.pgpu().args(args).output().unwrap();
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(
            said.starts_with(
                "pgpu: running alone is only for re-running a number that looks skewed"
            ),
            "{said}"
        );
    }
    let a = sb.run(
        &["--as", "alpha", "--class", "clip"],
        "a",
        &ticker("a", true),
    );
    until("a runs", 10, || running(&sb, a));
    let e = sb.run(
        &[
            "--as",
            "bench",
            "--class",
            "bench",
            "--alone",
            "9% off its baseline",
        ],
        "e",
        &ticker("e", false),
    );
    until("the re-run waits for an empty queue", 10, || {
        why(&sb, e).contains("waits for an empty queue")
    });
    let c = sb.run(
        &["--as", "gamma", "--class", "clip"],
        "c",
        &ticker("c", true),
    );
    until("c takes the free lane by day", 10, || {
        running(&sb, c) && size(&sb.path("c.ticks")) > 0
    });
    holds("a and c keep their lanes at their turns", 1.0, || {
        running(&sb, a) && running(&sb, c) && !sb.path("e.ticks").exists()
    });
    sb.fake("night", "1\n");
    let d = sb.run(
        &["--as", "delta", "--class", "clip"],
        "d",
        &ticker("d", false),
    );
    until("at night d waits behind the re-run", 10, || {
        why(&sb, d).contains("bench runs alone")
    });
    holds("running jobs keep their lanes at night", 1.5, || {
        running(&sb, a)
            && running(&sb, c)
            && !sb.path("e.ticks").exists()
            && !sb.path("d.ticks").exists()
    });
    for job in ["a", "c"] {
        fs::write(sb.path(&format!("{job}.finish")), "").unwrap();
    }
    for pid in [a, c] {
        assert!(sb.wait_exit(pid, 20).success());
    }
    until("the re-run runs once they end", 10, || {
        size(&sb.path("e.ticks")) > 0
    });
    holds("it runs alone", 1.0, || {
        running(&sb, e) && !sb.path("d.ticks").exists()
    });
    fs::write(sb.path("e.finish"), "").unwrap();
    assert!(sb.wait_exit(e, 10).success());
    let said = fs::read_to_string(sb.path("e.err")).unwrap();
    assert!(said.contains("with the card to itself"), "{said}");
    assert!(said.contains("re-run alone: 9% off its baseline"), "{said}");
    until("d runs once the re-run is done", 10, || running(&sb, d));
    fs::write(sb.path("d.finish"), "").unwrap();
    assert!(sb.wait_exit(d, 20).success());
    let stats = sb.pgpu().args(["stats", "--json"]).output().unwrap();
    let stats = Json::parse(String::from_utf8_lossy(&stats.stdout).trim());
    let bench = &stats.field("benches").items()[0];
    assert_eq!(bench.field("alone").text(), "9% off its baseline");
    assert_eq!(bench.field("shared").num(), 0.0);
}

#[test]
fn a_bench_shares_the_lanes_and_says_how_many_jobs_ran_beside_it() {
    let mut sb = Sandbox::new("lanes-bench-shares");
    lanes(&sb, 2);
    let a = sb.run(
        &["--as", "alpha", "--class", "clip"],
        "a",
        &ticker("a", true),
    );
    until("a runs", 10, || running(&sb, a));
    let e = sb.run(
        &["--as", "bench", "--class", "bench"],
        "e",
        &ticker("e", true),
    );
    until("the bench takes the second lane", 10, || {
        running(&sb, e) && size(&sb.path("e.ticks")) > 0
    });
    holds("both keep their lanes", 1.5, || {
        running(&sb, a) && running(&sb, e)
    });
    fs::write(sb.path("e.finish"), "").unwrap();
    assert!(sb.wait_exit(e, 10).success());
    let said = fs::read_to_string(sb.path("e.err")).unwrap();
    assert!(said.contains("beside 1 other job"), "{said}");
    assert!(said.contains("--alone"), "{said}");
    fs::write(sb.path("a.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 10).success());
}

#[test]
fn a_bench_idle_ten_seconds_lends_its_lanes_and_takes_them_back_freezing_the_borrower() {
    let mut sb = Sandbox::new("lanes-bench-lends");
    lanes(&sb, 1);
    sb.config("lend", "on\n");
    sb.fake("gpu.bench", "0\n");
    let e = sb.run(
        &["--as", "bench", "--class", "bench", "--alone", "skewed"],
        "e",
        "while [ ! -e \"$D/e.finish\" ]; do\necho x >> \"$D/e.ticks\"\n[ -e \"$D/e.quiet\" ] || \"$PGPU\" turn\nsleep 0.05\ndone\n",
    );
    until("the bench ticks", 10, || size(&sb.path("e.ticks")) > 0);
    let bench = cell_of(&sb, e);
    let c = sb.run(
        &["--as", "gamma", "--class", "interactive"],
        "c",
        &ticker("c", false),
    );
    until("c waits behind the bench", 10, || {
        sb.waits_for(c).as_deref() == Some("alone")
    });
    holds("the bench keeps the gpu at its turns", 2.0, || {
        running(&sb, e) && !sb.path("c.ticks").exists()
    });
    fs::write(sb.path("e.quiet"), "").unwrap();
    let quiet = Instant::now();
    until("status names the bench idling the card", 10, || {
        sb.status_text().contains("idling the card")
    });
    until("the bench lends its lanes", 20, || {
        sb.job_state(e).as_deref() == Some("lent")
    });
    let lent = quiet.elapsed();
    assert!(lent >= Duration::from_secs(9), "{lent:?}");
    until("c runs on the lent lanes", 10, || {
        running(&sb, c) && size(&sb.path("c.ticks")) > 0
    });
    assert!(sb.status_text().contains("idling the card: lent its lanes"));
    let borrower = cell_of(&sb, c);
    sb.fake("gpu.bench", "50\n");
    until("the bench takes its lanes back", 5, || running(&sb, e));
    until("the borrower is frozen", 5, || frozen(&borrower));
    let ticks = size(&sb.path("e.ticks"));
    holds("the bench runs on, never frozen", 1.0, || !frozen(&bench));
    assert!(size(&sb.path("e.ticks")) > ticks, "the bench kept ticking");
    assert_eq!(sb.job_state(c).as_deref(), Some("waiting"));
    fs::write(sb.path("e.finish"), "").unwrap();
    assert!(sb.wait_exit(e, 10).success());
    let said = fs::read_to_string(sb.path("e.err")).unwrap();
    assert!(said.contains("held the GPU alone"), "{said}");
    assert!(said.contains("the GPU was busy"), "{said}");
    assert!(said.contains("lent its lanes once"), "{said}");
    until("c runs once the bench is done", 10, || {
        running(&sb, c) && !frozen(&borrower)
    });
    fs::write(sb.path("c.finish"), "").unwrap();
    assert!(sb.wait_exit(c, 10).success());
    let said = fs::read_to_string(sb.path("c.err")).unwrap();
    assert!(said.contains("frozen or waiting for the GPU"), "{said}");
    let log = fs::read_to_string(sb.path("state/pito-gpu-queue/log.tsv")).unwrap();
    let ends: Vec<&str> = log.lines().filter_map(|l| l.split('\t').nth(6)).collect();
    assert!(ends.contains(&"lend") && ends.contains(&"reclaim"), "{log}");
}

#[test]
fn a_job_that_hands_over_at_a_turn_reads_the_wait_in_its_frozen_file() {
    let mut sb = Sandbox::new("lanes-frozen-turn");
    lanes(&sb, 1);
    let a = sb.run(
        &["--as", "alpha", "--class", "clip"],
        "a",
        "while [ ! -e \"$D/a.go\" ]; do sleep 0.02; done\n\"$PGPU\" turn\ncat \"$GPU_QUEUE_FROZEN\" > \"$D/a.frozen\"\n",
    );
    until("a runs", 10, || running(&sb, a));
    let b = sb.run(
        &["--as", "beta", "--class", "interactive"],
        "b",
        "sleep 1.5\n",
    );
    until("b waits", 10, || {
        sb.job_state(b).as_deref() == Some("waiting")
    });
    fs::write(sb.path("a.go"), "").unwrap();
    assert!(sb.wait_exit(b, 10).success());
    assert!(sb.wait_exit(a, 10).success());
    let ms: u64 = fs::read_to_string(sb.path("a.frozen"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!((1_400..10_000).contains(&ms), "{ms}");
}

fn killed(sb: &Sandbox) -> Vec<Json> {
    let out = sb.pgpu().args(["stats", "--json"]).output().unwrap();
    Json::parse(String::from_utf8(out.stdout).unwrap().trim())
        .field("killed")
        .items()
        .to_vec()
}

#[test]
fn a_stalled_job_is_stopped_exits_124_and_is_recorded() {
    let mut sb = Sandbox::new("lanes-stall");
    sb.fake("stall_ms", "1500\n");
    sb.fake("grace_ms", "500\n");
    let began = Instant::now();
    let hang = sb.run_command(
        &["--as", "hang", "--class", "clip"],
        "hang",
        &["sleep", "60"],
    );
    let status = sb.wait_exit(hang, 20);
    assert_eq!(status.code(), Some(124));
    assert!(began.elapsed() < Duration::from_secs(15));
    let err = fs::read_to_string(sb.path("hang.err")).unwrap();
    assert!(err.contains("pgpu: stopped hang (clip): stalled"), "{err}");
    let killed = killed(&sb);
    assert_eq!(killed.len(), 1);
    assert_eq!(killed[0].field("why").text(), "stall");
    assert_eq!(killed[0].field("as").text(), "hang");
    assert_eq!(killed[0].field("budget_ms").num(), 1_500_000.0);
    let log = lines(&sb.path("state/pito-gpu-queue/log.tsv"));
    assert!(log.iter().any(|l| l.ends_with("\tstall")), "{log:?}");
}

#[test]
fn a_job_past_twice_its_budget_is_stopped_with_its_budget_named() {
    let mut sb = Sandbox::new("lanes-budget");
    sb.fake("grace_ms", "500\n");
    let spin = sb.run_command(
        &["--as", "spin", "--budget", "1s"],
        "spin",
        &["sh", "-c", "while :; do :; done"],
    );
    assert_eq!(sb.wait_exit(spin, 20).code(), Some(124));
    let err = fs::read_to_string(sb.path("spin.err")).unwrap();
    assert!(err.contains("passed its 1s budget"), "{err}");
    assert!(err.contains("twice its 1s budget"), "{err}");
    assert_eq!(killed(&sb)[0].field("why").text(), "budget");
}

#[test]
fn a_job_past_its_memory_ceiling_is_killed_whole_and_recorded() {
    let mut sb = Sandbox::new("lanes-memory");
    let hog = sb.run_command(
        &["--as", "hog", "--class", "clip", "--memory", "64M"],
        "hog",
        &[
            "sh",
            "-c",
            "sleep 60 & awk 'BEGIN { s = sprintf(\"%400000000s\", \"\"); print length(s) }'; wait",
        ],
    );
    let status = sb.wait_exit(hog, 30);
    let err = fs::read_to_string(sb.path("hog.err")).unwrap();
    if err.contains("no memory ceiling") {
        eprintln!("skipped the memory test: no memory controller for the job's cgroup");
        return;
    }
    assert_eq!(status.code(), Some(137), "{err}");
    assert!(
        err.contains("pgpu: stopped hog (clip): out of memory at its 64M ceiling"),
        "{err}"
    );
    let killed = killed(&sb);
    assert_eq!(killed.len(), 1);
    assert_eq!(killed[0].field("why").text(), "memory");
    assert_eq!(killed[0].field("memory_bytes").num(), 67_108_864.0);
    let log = lines(&sb.path("state/pito-gpu-queue/log.tsv"));
    assert!(log.iter().any(|l| l.ends_with("\tmemory")), "{log:?}");
}

#[test]
fn the_lanes_stay_at_gpu_slots_however_idle_the_card_and_whatever_an_older_wrapper_grew() {
    let mut sb = Sandbox::new("lanes-fixed");
    lanes(&sb, 2);
    sb.fake("card_busy", "5\n");
    fs::create_dir_all(sb.path("run/pito-gpu-queue")).unwrap();
    fs::write(sb.path("run/pito-gpu-queue/load"), "4 0\n").unwrap();
    let names = ["a", "b", "c"];
    let mut jobs = Vec::new();
    for name in names {
        jobs.push(sb.run(
            &["--as", name, "--class", "clip"],
            name,
            &ticker(name, false),
        ));
    }
    let held = |sb: &Sandbox| jobs.iter().filter(|pid| running(sb, **pid)).count();
    until("two lanes fill", 10, || held(&sb) == 2);
    holds("never a third", 3.0, || held(&sb) == 2);
    let waiter = *jobs.iter().find(|pid| !running(&sb, **pid)).unwrap();
    assert!(
        why(&sb, waiter).contains("all 2 GPU lanes busy"),
        "{}",
        why(&sb, waiter)
    );
    let load = fs::read_to_string(sb.path("run/pito-gpu-queue/load")).unwrap();
    assert!(load.starts_with("2 "), "{load}");
    for name in names {
        fs::write(sb.path(&format!("{name}.finish")), "").unwrap();
    }
    for pid in jobs {
        assert!(sb.wait_exit(pid, 20).success());
    }
}

#[test]
fn a_waiter_that_never_takes_its_free_lane_is_passed_over_and_the_next_job_runs() {
    let mut sb = Sandbox::new("lanes-passed");
    lanes(&sb, 2);
    sb.fake("pass_ms", "1000\n");
    let a = sb.run(
        &["--as", "alpha", "--class", "clip"],
        "a",
        &ticker("a", false),
    );
    until("a runs", 10, || running(&sb, a));
    let mut stuck = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pid = stuck.id();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split(' ')
        .nth(19)
        .unwrap();
    fs::write(
        sb.path(&format!("run/pito-gpu-queue/tickets/1-1-{pid}")),
        format!("stuck\nan older wrapper\n\n\n{start}\nclip\n\n"),
    )
    .unwrap();
    let c = sb.run(
        &["--as", "gamma", "--class", "clip"],
        "c",
        &ticker("c", false),
    );
    until("c takes the lane the stuck waiter left", 15, || {
        running(&sb, c)
    });
    assert!(
        why(&sb, pid).starts_with("passed over"),
        "{}",
        why(&sb, pid)
    );
    let _ = stuck.kill();
    let _ = stuck.wait();
    for job in ["a", "c"] {
        fs::write(sb.path(&format!("{job}.finish")), "").unwrap();
    }
    assert!(sb.wait_exit(a, 10).success());
    assert!(sb.wait_exit(c, 10).success());
}
