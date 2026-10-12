mod common;

use std::fs;
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::*;

#[test]
fn the_queue_serves_class_first_then_the_least_recently_served_session_then_arrival() {
    let mut sb = Sandbox::new("order");
    for who in ["beta", "alpha"] {
        let pid = sb.run(&["--as", who], &format!("serve-{who}"), "true\n");
        assert!(sb.wait_exit(pid, 15).success());
        sleep(Duration::from_millis(50));
    }
    let holder = sb.run(&["--as", "zed"], "holder", &ticker("holder", false));
    until("the holder runs", 10, || {
        sb.job_state(holder).as_deref() == Some("running")
    });
    let log = "echo NAME >> \"$D/order.log\"\n";
    let plan: [(&str, &str, &str); 6] = [
        ("search", "alpha", "alpha-search"),
        ("clip", "alpha", "alpha-clip"),
        ("clip", "beta", "beta-clip-1"),
        ("interactive", "gamma", "gamma-still"),
        ("clip", "beta", "beta-clip-2"),
        ("clip", "delta", "delta-clip"),
    ];
    for (class, who, name) in plan {
        queue_and_wait(
            &mut sb,
            &["--class", class, "--as", who],
            name,
            &log.replace("NAME", name),
        );
    }
    fs::write(sb.path("holder.finish"), "").unwrap();
    let order = sb.path("order.log");
    until("every queued job has run", 40, || lines(&order).len() == 6);
    assert_eq!(
        lines(&order),
        [
            "gamma-still",
            "delta-clip",
            "beta-clip-1",
            "alpha-clip",
            "beta-clip-2",
            "alpha-search",
        ]
    );
}

#[test]
fn a_truth_job_that_has_waited_takes_turns_with_the_clips_and_keeps_its_class() {
    let mut sb = Sandbox::new("aging");
    sb.fake("age_ms", "1500\n");
    let alpha = sb.run(
        &["--class", "clip", "--as", "alpha"],
        "alpha",
        &ticker("alpha", true),
    );
    until("alpha runs", 10, || {
        sb.job_state(alpha).as_deref() == Some("running")
    });
    let beta = sb.run(
        &["--class", "clip", "--as", "beta"],
        "beta",
        &ticker("beta", true),
    );
    until("beta waits", 10, || {
        sb.job_state(beta).as_deref() == Some("waiting")
    });
    let bake = sb.run(
        &["--class", "truth", "--as", "studio"],
        "bake",
        "echo started > \"$D/bake.started\"\nwhile [ ! -e \"$D/bake.finish\" ]; do sleep 0.05; done\n",
    );
    until("the bake runs while both clips still turn", 20, || {
        sb.path("bake.started").exists()
    });
    assert!(!sb.path("alpha.done").exists() && !sb.path("beta.done").exists());
    let names = ticket_files(&sb, bake);
    assert!(names[0].starts_with("2-"), "{names:?}");
    let body =
        fs::read_to_string(sb.path(&format!("run/pito-gpu-queue/tickets/{}", names[0]))).unwrap();
    assert_eq!(body.lines().nth(2), Some("aged"), "{body:?}");
    assert_eq!(body.lines().nth(3), Some("ran"), "{body:?}");
    let job = job_json(&sb, bake).expect("the bake is queued");
    assert_eq!(job.field("state").text(), "running");
    assert_eq!(job.field("class").text(), "truth");
    let text = sb.status_text();
    assert!(
        text.lines().any(|line| line.starts_with("running  studio")
            && line.contains("waited 10 min: takes turns as clip")),
        "{text}"
    );
    fs::write(sb.path("bake.finish"), "").unwrap();
    assert!(sb.wait_exit(bake, 10).success());
    let log = lines(&sb.path("state/pito-gpu-queue/log.tsv"));
    let row: Vec<String> = log
        .iter()
        .find(|line| line.split('\t').nth(2) == Some("studio"))
        .expect("the bake's hold is logged")
        .split('\t')
        .map(str::to_string)
        .collect();
    assert_eq!(row[3], "2", "logged as truth: {row:?}");
    let waited: u128 = row[4].parse().unwrap();
    assert!(
        waited >= 1500,
        "the bake got in after {waited} ms, before it aged"
    );
    for name in ["alpha", "beta"] {
        fs::write(sb.path(&format!("{name}.finish")), "").unwrap();
    }
    assert!(sb.wait_exit(alpha, 20).success());
    assert!(sb.wait_exit(beta, 20).success());
}

#[test]
fn a_turn_hands_the_gpu_to_a_waiting_job_of_another_session_and_takes_it_back() {
    let mut sb = Sandbox::new("handover");
    let body = "while [ ! -e \"$D/handover.finish\" ]; do\necho a >> \"$D/handover.log\"\n\"$PGPU\" turn\nsleep 0.05\ndone\n";
    let a = sb.run(&["--class", "clip", "--as", "alpha"], "a", body);
    let log = sb.path("handover.log");
    until("a has run", 10, || !lines(&log).is_empty());
    let b = sb.run(
        &["--class", "clip", "--as", "beta"],
        "b",
        "echo b-start >> \"$D/handover.log\"\nsleep 1\necho b-end >> \"$D/handover.log\"\n",
    );
    until("a yields and b runs", 20, || {
        sb.job_state(a).as_deref() == Some("yielded")
            && sb.job_state(b).as_deref() == Some("running")
    });
    until("b ends", 10, || lines(&log).contains(&"b-end".to_string()));
    until("a gets the gpu back", 15, || {
        lines(&log).last().is_some_and(|last| last == "a")
    });
    let all = lines(&log);
    assert_eq!(all[0], "a");
    let start = all.iter().position(|l| l == "b-start").unwrap();
    let end = all.iter().position(|l| l == "b-end").unwrap();
    assert!(start > 0);
    assert_eq!(end, start + 1, "a ran while b held the gpu: {all:?}");
    fs::write(sb.path("handover.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 10).success());
    assert!(sb.wait_exit(b, 10).success());
}

#[test]
fn two_turns_called_at_once_from_one_job_both_come_back_and_the_queue_moves_on() {
    let mut sb = Sandbox::new("twin-turns");
    let body = "while [ ! -e \"$D/twin.finish\" ]; do\necho a >> \"$D/twin.log\"\n\"$PGPU\" turn &\n\"$PGPU\" turn --work-ms 40 --longest-ms 2 &\nwait\nsleep 0.05\ndone\n";
    let a = sb.run(&["--class", "clip", "--as", "alpha"], "a", body);
    let log = sb.path("twin.log");
    until("a has run", 10, || !lines(&log).is_empty());
    for round in 0..3 {
        let name = format!("b{round}");
        let b = sb.run(
            &["--class", "clip", "--as", "beta"],
            &name,
            &format!("echo {name}-start >> \"$D/twin.log\"\nsleep 0.5\necho {name}-end >> \"$D/twin.log\"\n"),
        );
        assert!(sb.wait_exit(b, 30).success(), "round {round}: b never ran");
        let end = format!("{name}-end");
        until("a gets the gpu back after both its turns", 15, || {
            let all = lines(&log);
            all.iter()
                .position(|l| *l == end)
                .is_some_and(|at| all[at + 1..].iter().any(|l| l == "a"))
        });
    }
    fs::write(sb.path("twin.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 15).success());
}

#[test]
fn sigterm_to_a_wrapper_resumes_its_stopped_tree_before_it_exits() {
    let mut sb = Sandbox::new("sigterm");
    let (wrapper, cell) = park_a_job_by_take_back(&mut sb, "term");
    signal(wrapper, libc::SIGTERM);
    assert_eq!(sb.wait_exit(wrapper, 10).code(), Some(143));
    assert!(!cell.exists(), "the job's cgroup outlived its wrapper");
    assert!(
        !sb.path(&format!("run/pito-gpu-queue/stopped/{wrapper}"))
            .exists()
    );
    until("nothing of the job is left", 3, || {
        group_states(wrapper).is_empty()
    });
}

#[test]
fn sigkill_to_a_wrapper_leaves_its_job_frozen_until_rescue_kills_it_in_its_cell() {
    let mut sb = Sandbox::new("sigkill");
    let (wrapper, cell) = park_a_job_by_take_back(&mut sb, "kill");
    let ticks = sb.path("kill.ticks");
    let entry = sb.path(&format!("run/pito-gpu-queue/stopped/{wrapper}"));
    signal(wrapper, libc::SIGKILL);
    sb.wait_exit(wrapper, 10);
    assert!(frozen(&cell), "the job was not left frozen");
    assert!(!group_states(wrapper).is_empty(), "the job died");
    let before = size(&ticks);
    sb.status();
    until("rescue kills the job", 15, || {
        group_states(wrapper).is_empty()
    });
    assert_eq!(
        size(&ticks),
        before,
        "the rescued job ran outside the queue"
    );
    assert!(!cell.exists(), "rescue left the dead wrapper's cgroup");
    assert!(!entry.exists());
}

#[test]
fn a_job_that_keeps_forking_is_frozen_whole_with_no_thread_left_running() {
    let mut sb = Sandbox::new("fork");
    sb.config("lend", "on\n");
    sb.fake("gpu.fork", "0\n");
    let body = "while [ ! -e \"$D/fork.finish\" ]; do\n( echo x >> \"$D/fork.ticks\"; sleep 0.1; echo y >> \"$D/fork.ticks\" ) &\nsleep 0.01\ndone\nwait\necho done > \"$D/fork.done\"\n";
    let wrapper = sb.run(&["--as", "fork"], "fork", body);
    let ticks = sb.path("fork.ticks");
    until("the job forks", 10, || size(&ticks) > 0);
    let cell = cell_of(&sb, wrapper);
    let other = lend_then_take_back(&mut sb, "fork", wrapper);
    until_still("the forking job is parked", 20, &ticks);
    assert!(frozen(&cell), "the cgroup is not frozen");
    let threads = cell_threads(&cell);
    assert!(threads.len() > 2, "too few processes caught: {threads:?}");
    assert!(
        threads
            .iter()
            .all(|(_, state)| !matches!(state, 'R' | 'T' | 't')),
        "a thread runs or is stopped while frozen: {threads:?}"
    );
    let still = size(&ticks);
    holds("a forked process ran while the job was frozen", 2.0, || {
        size(&ticks) == still
    });
    fs::write(sb.path("other.finish"), "").unwrap();
    assert!(sb.wait_exit(other, 15).success());
    until("the forking job runs again", 20, || size(&ticks) > still);
    fs::write(sb.path("fork.finish"), "").unwrap();
    assert!(sb.wait_exit(wrapper, 15).success());
    assert!(sb.path("fork.done").exists());
    assert!(!cell.exists(), "the job's cgroup outlived its wrapper");
}

#[test]
fn a_job_whose_cgroup_cannot_be_made_is_paused_with_sigstop_and_marked_so() {
    let Some(jail) = Jail::new("fallback") else {
        eprintln!("skipped the fallback test: no cgroup of its own could be made");
        return;
    };
    let mut sb = Sandbox::new("fallback");
    sb.config("lend", "on\n");
    sb.fake("gpu.fall", "0\n");
    let wrapper = sb.run_in(
        &jail.procs(),
        &["--as", "fall"],
        "fall",
        &ticker("fall", false),
    );
    let ticks = sb.path("fall.ticks");
    until("the job ticks", 10, || size(&ticks) > 0);
    let said = lines(&sb.path("fall.err"));
    assert_eq!(
        said.iter()
            .filter(|line| line.contains("SIGSTOP instead of the cgroup freezer"))
            .count(),
        1,
        "{said:?}"
    );
    assert!(!jail.path.join(format!("pgpu-{wrapper}")).exists());
    let entry = sb.path(&format!("run/pito-gpu-queue/stopped/{wrapper}"));
    assert!(!entry.exists());
    lend_then_take_back(&mut sb, "fall", wrapper);
    until_still("the job is parked", 20, &ticks);
    assert!(
        parked(wrapper) > 0,
        "the job did not stop: {:?}",
        group_states(wrapper)
    );
    let marked = lines(&entry);
    assert_eq!(marked.len(), 3, "{marked:?}");
    assert!(marked[0].split(' ').all(|token| token.contains(':')));
    assert_eq!(marked[2], "sigstop");
    signal(wrapper, libc::SIGTERM);
    assert_eq!(sb.wait_exit(wrapper, 10).code(), Some(143));
    assert!(!entry.exists());
    until("nothing of the job is left stopped", 3, || {
        parked(wrapper) == 0
    });
}

#[test]
fn status_json_has_every_field_ptop_reads() {
    let mut sb = Sandbox::new("status");
    let first = sb.run(
        &["--class", "truth", "--as", "shape test"],
        "first",
        "true\n",
    );
    assert!(sb.wait_exit(first, 15).success());
    let holder = sb.run(
        &["--class", "clip", "--as", "shape holder"],
        "holder",
        &ticker("holder", false),
    );
    until("the holder runs", 10, || {
        sb.job_state(holder).as_deref() == Some("running")
    });
    let waiter = sb.run(
        &["--class", "search", "--as", "shape-wait"],
        "waiter",
        &marker("waiter"),
    );
    until("the waiter is queued", 10, || sb.queued() == 2);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64;

    fs::remove_file(sb.path("run/pito-gpu-queue/hour")).unwrap();
    let status = sb.status();
    for key in [
        "v", "paused", "gpu_busy", "play", "away", "desk", "vram", "jobs", "hour", "outside",
    ] {
        status.field(key);
    }
    for row in status.field("outside").items() {
        assert!(row.field("pid").num() > 0.0);
        row.field("comm").text();
        assert!(row.field("gpu_pct").num() >= 5.0);
        assert!(row.field("vram_mib").num() >= 0.0);
        assert!(row.field("since").num() > 0.0);
        row.field("lane");
    }
    assert_eq!(status.field("v").num(), 1.0);
    assert!(!status.field("paused").flag());
    null_or_num(status.field("gpu_busy"));

    for gone in ["play", "away", "away_reason", "desk"] {
        assert!(status.field(gone).is_null(), "{gone}");
    }
    let vram = status.field("vram");
    if !vram.is_null() {
        assert!(vram.field("total_mib").num() >= vram.field("used_mib").num());
    }

    let hour = status.field("hour");
    for key in ["held_pct", "jobs", "holds", "gave_way"] {
        assert!(hour.field(key).num() >= 0.0);
    }
    assert!(hour.field("holds").num() >= 1.0);
    for key in ["p50", "p90", "max"] {
        assert!(hour.field("wait_ms").field(key).num() >= 0.0);
    }

    let jobs = status.field("jobs").items();
    assert_eq!(jobs.len(), 2);
    let (run, wait) = (&jobs[0], &jobs[1]);
    assert_eq!(run.field("pid").num() as u32, holder);
    assert_eq!(run.field("state").text(), "running");
    assert_eq!(run.field("position").num(), 0.0);
    assert_eq!(run.field("class").text(), "clip");
    assert_eq!(run.field("as").text(), "shape_holder");
    assert!(run.field("label").text().ends_with("holder.sh"));
    assert!(!run.field("realtime").flag());
    assert!((now_ms - run.field("arrived").num()).abs() < 120_000.0);
    assert!((now_ms - run.field("since").num()).abs() < 120_000.0);
    assert!(run.field("waits_for").is_null());
    assert!(
        run.field("pace").is_null(),
        "a job that never turned has no pace"
    );
    assert!(
        run.field("lane").is_null(),
        "a job outside pueue has no lane"
    );
    assert!(wait.field("pace").is_null());
    assert!(wait.field("lane").is_null());
    assert_eq!(wait.field("pid").num() as u32, waiter);
    assert_eq!(wait.field("state").text(), "waiting");
    assert_eq!(wait.field("position").num(), 1.0);
    assert_eq!(wait.field("class").text(), "search");
    assert_eq!(wait.field("as").text(), "shape_wait");
    assert!(!wait.field("realtime").flag());
    assert!(wait.get("since").is_none());
    assert!(wait.field("arrived").num() >= run.field("arrived").num());
    assert_eq!(wait.field("waits_for").text(), "turn");
    assert!(
        wait.field("why").text().starts_with("the GPU is busy"),
        "{}",
        wait.field("why").text()
    );
    assert!(run.field("why").is_null());
    assert!(!run.field("alone").flag() && !wait.field("alone").flag());
    assert!(wait.field("budget_ms").is_null() && wait.field("over_ms").is_null());

    let paused = sb.pgpu().arg("pause").output().unwrap();
    assert!(paused.status.success());
    let status = sb.status();
    assert!(status.field("paused").flag());
    assert_eq!(
        status.field("jobs").items()[1].field("waits_for").text(),
        "paused"
    );
    let resumed = sb.pgpu().arg("resume").output().unwrap();
    assert!(resumed.status.success());
    assert!(!sb.status().field("paused").flag());
    fs::write(sb.path("holder.finish"), "").unwrap();
    assert!(sb.wait_exit(holder, 15).success());
    assert!(sb.wait_exit(waiter, 15).success());
}

#[test]
fn two_jobs_of_one_session_and_class_take_turns_instead_of_waiting_for_the_end() {
    let mut sb = Sandbox::new("same-session");
    let args = ["--class", "clip", "--as", "same"];
    let a = sb.run(&args, "a", &ticker("a", true));
    let a_ticks = sb.path("a.ticks");
    until("a ticks", 10, || size(&a_ticks) > 0);
    let queued = Instant::now();
    let b = sb.run(&args, "b", &marker("b"));
    until("b starts while a is still running", 12, || {
        sb.path("b.started").exists()
    });
    assert!(
        queued.elapsed() < Duration::from_secs(10),
        "b waited {:?} behind a job of its own session",
        queued.elapsed()
    );
    assert!(!sb.path("a.done").exists(), "a ended before b started");
    until("b ends", 10, || sb.path("b.done").exists());
    let before = size(&a_ticks);
    until("a gets the gpu back and ticks again", 15, || {
        size(&a_ticks) > before
    });
    fs::write(sb.path("a.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 15).success());
    assert!(sb.wait_exit(b, 10).success());
}

#[test]
fn sigterm_to_a_wrapper_that_gave_way_at_a_turn_exits_143_promptly_and_stops_its_job() {
    let mut sb = Sandbox::new("term-yielded");
    let (a, b) = yield_a_to_b(&mut sb);
    let job = job_pid(&sb, "a");
    assert!(!gone(job));
    let sent = Instant::now();
    signal(a, libc::SIGTERM);
    let status = sb.wait_exit(a, 6);
    assert!(
        sent.elapsed() < Duration::from_secs(5),
        "the wrapper took {:?} to leave",
        sent.elapsed()
    );
    assert_eq!(status.code(), Some(143));
    until("the job is gone", 5, || gone(job));
    assert_eq!(sb.job_state(a), None);
    assert!(ticket_files(&sb, a).is_empty());
    assert_eq!(sb.job_state(b).as_deref(), Some("running"));
    fs::write(sb.path("b.finish"), "").unwrap();
    assert!(sb.wait_exit(b, 10).success());
}

#[test]
fn sigterm_to_a_wrapper_still_waiting_for_its_first_start_exits_143_and_leaves_no_ticket() {
    let mut sb = Sandbox::new("term-waiting");
    let holder = sb.run(&["--as", "zed"], "holder", &ticker("holder", false));
    until("the holder runs", 10, || {
        sb.job_state(holder).as_deref() == Some("running")
    });
    let waiter = queue_and_wait(&mut sb, &["--as", "later"], "later", &marker("later"));
    assert_eq!(sb.job_state(waiter).as_deref(), Some("waiting"));
    assert!(!ticket_files(&sb, waiter).is_empty());
    let sent = Instant::now();
    signal(waiter, libc::SIGTERM);
    let status = sb.wait_exit(waiter, 6);
    assert!(
        sent.elapsed() < Duration::from_secs(5),
        "the wrapper took {:?} to leave",
        sent.elapsed()
    );
    assert_eq!(status.code(), Some(143));
    assert_eq!(sb.job_state(waiter), None);
    assert!(ticket_files(&sb, waiter).is_empty());
    assert!(!sb.path("later.started").exists());
    assert_eq!(sb.job_state(holder).as_deref(), Some("running"));
    fs::write(sb.path("holder.finish"), "").unwrap();
    assert!(sb.wait_exit(holder, 10).success());
}

#[test]
fn a_wrapper_killed_with_sigkill_leaves_no_running_holder_in_status() {
    let mut sb = Sandbox::new("dead-holder");
    let wrapper = sb.run(&["--as", "doomed"], "doomed", &ticker("doomed", false));
    until("the job runs", 10, || {
        sb.job_state(wrapper).as_deref() == Some("running")
    });
    assert!(sb.path("run/pito-gpu-queue/running").exists());
    signal(wrapper, libc::SIGKILL);
    sb.wait_exit(wrapper, 10);
    until("status shows no running holder", 10, || {
        sb.status()
            .field("jobs")
            .items()
            .iter()
            .all(|job| job.field("state").text() != "running")
    });
    let text = sb.status_text();
    assert!(
        !text.contains("doomed"),
        "plain status shows a holder: {text}"
    );
    assert!(!sb.path("run/pito-gpu-queue/running").exists());
    assert_eq!(sb.queued(), 0);
}

#[test]
fn a_command_with_a_newline_in_an_argument_gets_a_one_line_label_and_runs_as_an_ordinary_job() {
    let mut sb = Sandbox::new("newline");
    let code = "echo started > \"$D/nl.started\"\nsleep 2\nrealtime=no\necho done > \"$D/nl.done\"";
    let wrapper = sb.run_command(
        &["--class", "clip", "--as", "nl"],
        "nl",
        &["sh", "-c", code],
    );
    until("the job runs", 10, || {
        sb.job_state(wrapper).as_deref() == Some("running")
    });
    let status = sb.status();
    let jobs = status.field("jobs").items();
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    let label = job.field("label").text();
    assert!(!label.contains('\n') && !label.contains('\r'), "{label:?}");
    assert!(
        label.contains("sleep 2") && label.contains("realtime=no"),
        "{label:?}"
    );
    assert!(!job.field("realtime").flag());
    assert_eq!(job.field("position").num(), 0.0);
    assert_eq!(job.field("class").text(), "clip");
    let running = fs::read_to_string(sb.path("run/pito-gpu-queue/running")).unwrap();
    let held: Vec<&str> = running.lines().collect();
    assert_eq!(held[0], "nl");
    assert_eq!(held[2], wrapper.to_string());
    let second = sb.run(
        &["--class", "clip", "--as", "after"],
        "after",
        &marker("after"),
    );
    until("the job behind it waits", 10, || {
        sb.job_state(second).as_deref() == Some("waiting")
    });
    until("the job ends", 15, || sb.path("nl.done").exists());
    assert!(sb.wait_exit(wrapper, 10).success());
    until("the job behind it runs", 15, || {
        sb.path("after.done").exists()
    });
    assert!(sb.wait_exit(second, 10).success());
}

#[test]
fn killing_a_wrapper_and_its_job_at_a_turn_together_leaves_no_turn_file_after_the_next_call() {
    let mut sb = Sandbox::new("stale-turn");
    let (a, b) = yield_a_to_b(&mut sb);
    let cell = cell_of(&sb, a);
    let turn_file = sb.path(&format!("run/pito-gpu-queue/turns/{a}"));
    until("a waits in turn", 5, || turn_file.exists());
    unsafe {
        libc::kill(-(a as libc::pid_t), libc::SIGKILL);
    }
    sb.wait_exit(a, 10);
    until("the stale turn file is dropped", 10, || {
        sb.status();
        !turn_file.exists()
    });
    assert!(!cell.exists(), "rescue left the dead wrapper's cgroup");
    assert!(ticket_files(&sb, a).is_empty());
    assert_eq!(sb.job_state(b).as_deref(), Some("running"));
    fs::write(sb.path("b.finish"), "").unwrap();
    assert!(sb.wait_exit(b, 10).success());
}

#[test]
fn a_paused_queue_lets_a_job_that_gave_way_come_back_and_finish_but_starts_no_new_job() {
    let mut sb = Sandbox::new("paused-return");
    let (a, b) = yield_a_to_b(&mut sb);
    let paused = sb.pgpu().arg("pause").output().unwrap();
    assert!(paused.status.success());
    let c = queue_and_wait(
        &mut sb,
        &["--class", "clip", "--as", "gamma"],
        "c",
        &marker("c"),
    );
    fs::write(sb.path("b.finish"), "").unwrap();
    assert!(sb.wait_exit(b, 10).success());
    let ticks = sb.path("a.ticks");
    let before = size(&ticks);
    until("a comes back while the queue is paused", 15, || {
        size(&ticks) > before
    });
    assert_eq!(sb.job_state(a).as_deref(), Some("running"));
    assert_eq!(sb.job_state(c).as_deref(), Some("waiting"));
    fs::write(sb.path("a.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 15).success());
    holds("a new job started while the queue was paused", 2.0, || {
        !sb.path("c.started").exists()
    });
    assert_eq!(sb.job_state(c).as_deref(), Some("waiting"));
    let resumed = sb.pgpu().arg("resume").output().unwrap();
    assert!(resumed.status.success());
    until("the new job runs after resume", 15, || {
        sb.path("c.done").exists()
    });
    assert!(sb.wait_exit(c, 10).success());
}

#[test]
fn a_job_ended_by_a_signal_makes_pgpu_run_exit_128_plus_it() {
    let mut sb = Sandbox::new("signal-exit");
    let killed = sb.run(&["--as", "killed"], "killed", "kill -9 $$\n");
    assert_eq!(sb.wait_exit(killed, 15).code(), Some(137));
    let termed = sb.run(&["--as", "termed"], "termed", "kill -TERM $$\n");
    assert_eq!(sb.wait_exit(termed, 15).code(), Some(143));
    let plain = sb.run(&["--as", "plain"], "plain", "exit 7\n");
    assert_eq!(sb.wait_exit(plain, 15).code(), Some(7));
}

#[test]
fn a_job_that_reports_its_pace_shows_it_and_its_lane_in_status_and_the_pace_log() {
    let mut sb = Sandbox::new("pace-report");
    sb.env = vec![
        ("PUEUE_GROUP".to_string(), "gpu".to_string()),
        ("PUEUE_WORKER_ID".to_string(), "0".to_string()),
    ];
    let paced = sb.run(
        &["--class", "clip", "--as", "paced"],
        "paced",
        &reporter_every(
            "paced",
            "--work-ms 40 --longest-ms 3.5 --bogus x",
            0.1,
            0.05,
        ),
    );
    sb.env.clear();
    let ticks = sb.path("paced.ticks");
    until("the job turns", 10, || size(&ticks) > 30);
    let job = job_json(&sb, paced).unwrap();
    let pace = job.field("pace");
    assert_eq!(pace.field("longest_ms").num(), 3.5, "{pace:?}");
    assert_eq!(pace.field("work_ms").num(), 40.0, "{pace:?}");
    let gap = pace.field("turn_gap_ms").num();
    assert!(gap > 0.0 && gap < 5_000.0, "{pace:?}");
    let lane = job.field("lane");
    assert_eq!(lane.field("group").text(), "gpu");
    assert_eq!(lane.field("worker").num(), 0.0);
    let path = lane.field("path");
    let started = lane.field("started");
    assert_eq!(
        path.is_null(),
        started.is_null(),
        "both or neither, whether or not the test itself runs under pueued: {lane:?}"
    );
    null_or_num(started);
    if !path.is_null() {
        assert!(path.text().starts_with('/'), "{lane:?}");
        let started = started.num();
        assert!(started > 1.7e12 && started < now_ms_of_test(), "{lane:?}");
    }
    let ring = lines(&sb.path(&format!("run/pito-gpu-queue/turns/{paced}.pace")));
    assert!(!ring.is_empty() && ring.len() <= 64, "{ring:?}");
    assert!(
        ring.iter().all(|l| l.split('\t').nth(2) == Some("3.5")),
        "{ring:?}"
    );
    let text = sb.status_text();
    assert!(
        text.contains("pace: longest step 3.5 ms, turn gap"),
        "{text}"
    );
    assert!(text.contains("lane gpu 0"), "{text}");
    fs::write(sb.path("paced.finish"), "").unwrap();
    assert!(sb.wait_exit(paced, 10).success());
    assert!(
        !sb.path(&format!("run/pito-gpu-queue/turns/{paced}.pace"))
            .exists()
    );
    let plain = sb.run(&["--as", "plain"], "plain", &ticker("plain", true));
    until("the plain job turns", 10, || {
        size(&sb.path("plain.ticks")) > 30
    });
    let job = job_json(&sb, plain).unwrap();
    let pace = job.field("pace");
    assert!(pace.field("work_ms").is_null(), "{pace:?}");
    assert!(pace.field("longest_ms").is_null(), "{pace:?}");
    assert!(pace.field("turn_gap_ms").num() >= 0.0, "{pace:?}");
    assert!(job.field("lane").is_null());
    fs::write(sb.path("plain.finish"), "").unwrap();
    assert!(sb.wait_exit(plain, 10).success());
    let logged = pace_lines(&sb, "paced");
    assert_eq!(logged.len(), 1, "{logged:?}");
    let f = &logged[0];
    assert_eq!(f.len(), 11, "{f:?}");
    assert_eq!(f[2], "1");
    let turns: u64 = f[3].parse().unwrap();
    assert!(turns > 6, "{f:?}");
    assert_eq!(f[4], f[3], "every turn reported: {f:?}");
    assert_eq!(&f[8..], ["3.5", "3.5", "3.5"], "{f:?}");
    let plain_log = pace_lines(&sb, "plain");
    assert_eq!(plain_log[0][4], "0");
    assert_eq!(&plain_log[0][8..], ["-", "-", "-"]);
    let out = sb
        .pgpu()
        .args(["stats", "--pace", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stats = Json::parse(String::from_utf8(out.stdout).unwrap().trim());
    let sessions = stats.field("sessions").items();
    let row = sessions
        .iter()
        .find(|s| s.field("as").text() == "paced")
        .unwrap();
    assert_eq!(row.field("jobs").num(), 1.0);
    assert_eq!(row.field("reported_pct").num(), 100.0);
    assert_eq!(row.field("longest_ms").field("max").num(), 3.5);
    let plain_row = sessions
        .iter()
        .find(|s| s.field("as").text() == "plain")
        .unwrap();
    assert!(plain_row.field("longest_ms").is_null());
    let text = sb.pgpu().args(["stats", "--pace"]).output().unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("paced") && text.contains("100%"), "{text}");
}

#[test]
fn a_job_that_stops_turning_and_leaves_the_gpu_idle_is_not_held_by_its_last_report() {
    let mut sb = Sandbox::new("pace-tail");
    sb.config("desk_guard", "on\n");
    sb.fake("away", "0\n");
    sb.fake("busy", "0\n");
    sb.fake("gpu.tail", "0\n");
    let body = "i=0\nwhile [ $i -lt 40 ]; do\n\"$PGPU\" turn --work-ms 45 --longest-ms 4\nsleep 0.05\ni=$((i+1))\ndone\necho tail > \"$D/tail.mark\"\nwhile [ ! -e \"$D/tail.finish\" ]; do\necho x >> \"$D/tail.ticks\"\nsleep 0.1\ndone\n";
    let tail = sb.run(&["--as", "tail"], "tail", body);
    until("the job stops turning", 20, || {
        sb.path("tail.mark").exists()
    });
    holds(
        "a job whose GPU went idle after its last turn was held at the desk",
        5.0,
        || !desk_held(&sb, tail) && sb.job_state(tail).as_deref() == Some("running"),
    );
    fs::write(sb.path("tail.finish"), "").unwrap();
    assert!(sb.wait_exit(tail, 10).success());
}

#[test]
fn a_holder_that_doesnt_use_the_gpu_lends_it_keeps_running_and_takes_it_back_frozen() {
    let mut sb = Sandbox::new("lend");
    let (idle, busy) = lend_to_a_busy_waiter(&mut sb, "idle", &ticker("idle", false));
    let ticks = sb.path("idle.ticks");
    let cell = cell_of(&sb, idle);
    let job = job_json(&sb, idle).unwrap();
    assert!(job.field("waits_for").is_null());
    let text = sb.status_text();
    assert!(
        text.contains("lent     idle  sh ")
            && text.contains("(not using the GPU; lent it to the next job)"),
        "{text}"
    );
    let at = size(&ticks);
    until("the lent job keeps running", 5, || size(&ticks) > at + 10);
    assert!(!frozen(&cell));
    sb.fake("gpu.idle", "50\n");
    until(
        "the lent job asks for the gpu back and is frozen",
        10,
        || frozen(&cell) && sb.job_state(idle).as_deref() == Some("waiting"),
    );
    assert!(!lent_mark(&sb, idle).exists());
    let held = size(&ticks);
    holds("the job ran before it held the gpu again", 2.0, || {
        size(&ticks) == held && sb.job_state(busy).as_deref() == Some("running")
    });
    fs::write(sb.path("busy.finish"), "").unwrap();
    assert!(sb.wait_exit(busy, 10).success());
    until("the job holds the gpu again and resumes", 10, || {
        sb.job_state(idle).as_deref() == Some("running") && size(&ticks) > held
    });
    assert!(!frozen(&cell));
    let log = lines(&sb.path("state/pito-gpu-queue/log.tsv"));
    assert_eq!(
        log.iter()
            .filter(|line| line.contains("\tidle\t") && line.ends_with("\tlend"))
            .count(),
        1,
        "{log:?}"
    );
    let _ = fs::remove_file(sb.path("run/pito-gpu-queue/hour"));
    let hour = sb.status().field("hour").clone();
    assert!(hour.field("gave_way").num() >= 1.0, "{hour:?}");
    fs::write(sb.path("idle.finish"), "").unwrap();
    assert!(sb.wait_exit(idle, 10).success());
}

#[test]
fn a_lent_job_that_exits_ends_its_wrapper_cleanly() {
    let mut sb = Sandbox::new("lend-exit");
    let (short, busy) = lend_to_a_busy_waiter(&mut sb, "short", &ticker("short", false));
    fs::write(sb.path("short.finish"), "").unwrap();
    assert!(sb.wait_exit(short, 10).success());
    assert!(!lent_mark(&sb, short).exists());
    assert_eq!(sb.job_state(short), None);
    assert_eq!(sb.job_state(busy).as_deref(), Some("running"));
    let log = lines(&sb.path("state/pito-gpu-queue/log.tsv"));
    assert!(
        log.iter()
            .all(|line| !line.contains("\tshort\t") || line.ends_with("\tlend")),
        "a lent job logged a second hold: {log:?}"
    );
    fs::write(sb.path("busy.finish"), "").unwrap();
    assert!(sb.wait_exit(busy, 10).success());
}

#[test]
fn a_lent_job_calling_turn_blocks_until_it_holds_the_gpu_again() {
    let mut sb = Sandbox::new("lend-turn");
    let body = "while [ ! -e \"$D/turner.go\" ]; do\necho x >> \"$D/turner.ticks\"\nsleep 0.05\ndone\n\"$PGPU\" turn\necho turned > \"$D/turner.turned\"\n";
    let (turner, busy) = lend_to_a_busy_waiter(&mut sb, "turner", body);
    fs::write(sb.path("turner.go"), "").unwrap();
    until("the lent job asks for the gpu back", 5, || {
        sb.job_state(turner).as_deref() == Some("yielded") && !lent_mark(&sb, turner).exists()
    });
    holds("the turn returned before the job held the gpu", 2.0, || {
        !sb.path("turner.turned").exists() && sb.job_state(busy).as_deref() == Some("running")
    });
    fs::write(sb.path("busy.finish"), "").unwrap();
    assert!(sb.wait_exit(busy, 10).success());
    until("the turn returns once the job holds the gpu", 10, || {
        sb.path("turner.turned").exists()
    });
    assert!(sb.wait_exit(turner, 10).success());
}
