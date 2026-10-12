mod common;

use std::fs;

use common::*;

fn stopped(pid: u32) -> bool {
    stat_of(pid).is_some_and(|(state, _)| state == 'T' || state == 't')
}

#[test]
fn a_stopped_wrapper_at_the_head_is_passed_over_and_runs_after_it_continues() {
    let mut sb = Sandbox::new("queue-stopped");
    let a = sb.run(&["--as", "alpha"], "a", &ticker("a", false));
    until("a runs", 10, || {
        sb.job_state(a).as_deref() == Some("running")
    });
    let b = queue_and_wait(&mut sb, &["--as", "beta"], "b", &marker("b"));
    let c = queue_and_wait(&mut sb, &["--as", "gamma"], "c", &ticker("c", false));
    let order: Vec<u32> = sb
        .status()
        .field("jobs")
        .items()
        .iter()
        .map(|job| job.field("pid").num() as u32)
        .collect();
    assert_eq!(order, vec![a, b, c], "b is next after a");
    signal(b, libc::SIGSTOP);
    until("b's wrapper is stopped", 10, || stopped(b));
    until("status says b is stopped", 10, || {
        sb.waits_for(b).as_deref() == Some("stopped")
    });
    assert_eq!(sb.job_state(b).as_deref(), Some("waiting"));
    assert_eq!(sb.waits_for(c).as_deref(), Some("turn"));
    fs::write(sb.path("a.finish"), "").unwrap();
    assert!(sb.wait_exit(a, 10).success());
    until("c runs past the stopped b", 10, || {
        sb.job_state(c).as_deref() == Some("running")
    });
    assert!(!sb.path("b.started").exists());
    signal(b, libc::SIGCONT);
    until("b's wrapper runs again", 10, || !stopped(b));
    until("status no longer says b is stopped", 10, || {
        sb.waits_for(b).as_deref() == Some("turn")
    });
    holds("b waits for c, which holds the GPU", 1.0, || {
        !sb.path("b.started").exists() && sb.job_state(c).as_deref() == Some("running")
    });
    fs::write(sb.path("c.finish"), "").unwrap();
    assert!(sb.wait_exit(c, 10).success());
    assert!(sb.wait_exit(b, 10).success());
    assert!(sb.path("b.done").exists());
}

#[test]
fn an_empty_or_dotted_session_name_is_stamped_as_unknown() {
    let mut sb = Sandbox::new("queue-unknown");
    for (at, name) in ["", ".", ".."].into_iter().enumerate() {
        let job = format!("job{at}");
        let pid = sb.run(&["--as", name], &job, &marker(&job));
        assert!(sb.wait_exit(pid, 15).success(), "--as {name:?}");
        assert!(sb.path(&format!("{job}.done")).exists());
    }
    let served: Vec<String> = fs::read_dir(sb.path("run/pito-gpu-queue/served"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(served, vec!["unknown".to_string()]);
    let stamp = fs::read_to_string(sb.path("run/pito-gpu-queue/served/unknown")).unwrap();
    assert!(
        stamp.trim().parse::<u128>().is_ok_and(|v| v > 0),
        "{stamp:?}"
    );
    let log = lines(&sb.path("state/pito-gpu-queue/log.tsv"));
    assert_eq!(
        log.iter()
            .filter(|line| line.contains("\tunknown\t"))
            .count(),
        3,
        "{log:?}"
    );
}
