use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, UNIX_EPOCH};

use crate::pace;
use crate::paths::{alive_as, home, now, turn_file, write_atomic};
use crate::queue::{Gate, Ticket, lent_marked, rank, rerun, tickets};
use crate::run::TURN_TICK;
use crate::seats::{self, Line, holders};
use crate::wake::{Bell, RING, back_path, ring_wrapper};

const SLICE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Decision {
    Keep,
    HandOver,
    Lent,
}

pub(crate) struct Seen {
    pub(crate) me: u32,
    pub(crate) tickets: Vec<Ticket>,
    pub(crate) paused: bool,
    pub(crate) lent: bool,
    pub(crate) slice_age: Option<Duration>,
}

pub(crate) fn decide(
    seen: Seen,
    open: impl Fn(u32) -> bool,
    within: impl FnOnce(Vec<Ticket>) -> bool,
) -> Decision {
    if seen.lent {
        return Decision::Lent;
    }
    if seen.tickets.iter().any(|t| t.pid == seen.me && t.alone) {
        return Decision::Keep;
    }
    let list: Vec<Ticket> = seen
        .tickets
        .into_iter()
        .filter(|t| open(t.pid) && (!seen.paused || t.started))
        .collect();
    if let Some(class) = list.iter().find(|t| t.pid == seen.me).map(Ticket::ranks_as) {
        let fresh = seen.slice_age.is_some_and(|age| age < SLICE);
        let higher = list.iter().any(|t| t.ranks_as() < class);
        if (!fresh || higher) && !within(list) {
            return Decision::HandOver;
        }
    }
    Decision::Keep
}

pub(crate) fn turn(args: &[String]) -> ExitCode {
    let (Some(path), Some(wrapper)) = (
        std::env::var_os("GPU_QUEUE_TURN").map(PathBuf::from),
        std::env::var("GPU_QUEUE_PID")
            .ok()
            .and_then(|v| v.parse::<u32>().ok()),
    ) else {
        return ExitCode::SUCCESS;
    };
    let dir = home();
    let mut raw = tickets(&dir);
    let Some((start, began)) = raw
        .iter()
        .find(|t| t.pid == wrapper)
        .map(|t| (t.start, started_at(&dir, t)))
    else {
        return ExitCode::SUCCESS;
    };
    let Some((_alone, waited)) = one_at_a_time(&dir, wrapper, start) else {
        return ExitCode::SUCCESS;
    };
    if waited {
        raw = tickets(&dir);
    }
    let frozen = pace::note(&dir, wrapper, pace::report(args), now(), began).frozen;
    let Some(me) = raw.iter().find(|t| t.pid == wrapper).cloned() else {
        return ExitCode::SUCCESS;
    };
    let lent_now = || !lent_marked(&dir, std::slice::from_ref(&me)).is_empty();
    let gate = Gate::read(&dir, &raw);
    let holding = crate::seats::holding(&dir);
    let waits = raw.iter().any(|t| rerun(t, &holding)) && crate::queue::night();
    let skip: Vec<u32> = raw
        .iter()
        .filter(|t| rerun(t, &holding) || (waits && !t.started))
        .map(|t| t.pid)
        .collect();
    let seen = Seen {
        me: wrapper,
        paused: dir.join("paused").exists(),
        lent: lent_now(),
        slice_age: slice_age(&dir, wrapper),
        tickets: raw,
    };
    let count = seats::room(&dir, seats::count(), holding.len());
    let decision = decide(
        seen,
        |pid| gate.open(pid) && !skip.contains(&pid),
        |list| {
            let ranked = rank(&dir, list, Some((&me.owner, wrapper, now())));
            let open: Vec<&Ticket> = ranked.iter().collect();
            Line {
                open: &open,
                holding: &[],
                room: count,
            }
            .within(wrapper)
            .is_ok()
        },
    );
    match decision {
        Decision::Lent | Decision::HandOver => {
            give_way(&dir, &path, wrapper, start, frozen);
            ExitCode::SUCCESS
        }
        Decision::Keep => back(&dir, &path, wrapper, start, lent_now(), frozen),
    }
}

fn started_at(dir: &Path, t: &Ticket) -> Option<u128> {
    if !t.started {
        return None;
    }
    fs::metadata(
        dir.join("tickets")
            .join(format!("{}-{}-{}", t.class, t.arrived, t.pid)),
    )
    .and_then(|m| m.modified())
    .ok()?
    .duration_since(UNIX_EPOCH)
    .ok()
    .map(|d| d.as_nanos())
}

fn slice_age(dir: &Path, wrapper: u32) -> Option<Duration> {
    let since = holders(dir).into_iter().find(|h| h.pid == wrapper)?.since?;
    Some(Duration::from_millis(
        u64::try_from(crate::paths::millis().saturating_sub(since)).unwrap_or(0),
    ))
}

fn back(
    dir: &Path,
    path: &Path,
    wrapper: u32,
    start: u64,
    lent: bool,
    frozen: Option<u64>,
) -> ExitCode {
    if lent {
        give_way(dir, path, wrapper, start, frozen);
    } else {
        let _ = write_atomic(&turn_file(dir, wrapper, ".last"), &now().to_string());
        pace::returned(dir, wrapper, frozen);
    }
    ExitCode::SUCCESS
}

fn give_way(dir: &Path, path: &Path, wrapper: u32, start: u64, frozen: Option<u64>) {
    let back = back_path(dir, wrapper);
    let bell = Bell::make(&back);
    if write_atomic(path, "wait").is_ok() {
        ring_wrapper(dir, wrapper, RING);
        wait_for_go(dir, path, wrapper, start, frozen, bell.as_ref());
    }
    if bell.is_some() {
        drop(bell);
        let _ = fs::remove_file(&back);
    }
}

fn one_at_a_time(dir: &Path, wrapper: u32, start: u64) -> Option<(Option<File>, bool)> {
    let Ok(file) = File::options()
        .create(true)
        .append(true)
        .open(turn_file(dir, wrapper, ".lock"))
    else {
        return Some((None, false));
    };
    let mut waited = false;
    loop {
        if file.try_lock().is_ok() {
            return Some((Some(file), waited));
        }
        if !alive_as(wrapper, start) {
            return None;
        }
        waited = true;
        sleep(TURN_TICK);
    }
}

fn wait_for_go(
    dir: &Path,
    path: &Path,
    wrapper: u32,
    start: u64,
    frozen: Option<u64>,
    bell: Option<&Bell>,
) {
    while alive_as(wrapper, start) {
        match fs::read_to_string(path) {
            Ok(body) if body.trim() == "go" => break,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let _ = write_atomic(path, "wait");
            }
            _ => {}
        }
        match bell {
            Some(bell) => {
                bell.wait(None, TURN_TICK);
            }
            None => sleep(TURN_TICK),
        }
    }
    let _ = fs::remove_file(path);
    if alive_as(wrapper, start) {
        let _ = write_atomic(&turn_file(dir, wrapper, ".last"), &now().to_string());
        pace::returned(dir, wrapper, frozen);
    }
}

#[cfg(test)]
mod tests {
    use super::{Decision, Seen, decide};
    use crate::queue::Ticket;
    use crate::seats::Line;
    use std::time::Duration;

    const ME: u32 = 100;
    const OTHER: u32 = 200;

    fn ticket(class: u8, pid: u32, started: bool) -> Ticket {
        Ticket {
            started,
            ..Ticket::bare(class, pid, &format!("o{pid}"))
        }
    }

    fn seen(other_class: u8, slice_s: f64) -> Seen {
        Seen {
            me: ME,
            tickets: vec![ticket(1, ME, true), ticket(other_class, OTHER, false)],
            paused: false,
            lent: false,
            slice_age: Some(Duration::from_secs_f64(slice_s)),
        }
    }

    fn other_first(list: Vec<Ticket>) -> bool {
        list.iter()
            .find(|t| t.pid == OTHER)
            .or(list.first())
            .is_none_or(|t| t.pid == ME)
    }

    fn by_class(list: Vec<Ticket>) -> bool {
        list.iter()
            .min_by_key(|t| (t.ranks_as(), t.pid))
            .is_none_or(|t| t.pid == ME)
    }

    fn lanes(room: usize) -> impl FnOnce(Vec<Ticket>) -> bool {
        move |list: Vec<Ticket>| {
            let mut list = list;
            list.sort_by_key(|t| (t.ranks_as(), t.pid == ME));
            let open: Vec<&Ticket> = list.iter().collect();
            Line {
                open: &open,
                holding: &[],
                room,
            }
            .within(ME)
            .is_ok()
        }
    }

    #[test]
    fn with_two_lanes_a_job_keeps_the_gpu_beside_another_and_hands_over_once_a_third_waits() {
        let two = |other: Ticket| Seen {
            tickets: vec![ticket(1, ME, true), other],
            ..seen(1, 9.0)
        };
        assert_eq!(
            decide(two(ticket(1, OTHER, true)), open, lanes(2)),
            Decision::Keep
        );
        assert_eq!(
            decide(two(ticket(1, OTHER, true)), open, lanes(1)),
            Decision::HandOver
        );
        let three = Seen {
            tickets: vec![
                ticket(1, ME, true),
                ticket(1, OTHER, true),
                ticket(1, 300, false),
            ],
            ..seen(1, 9.0)
        };
        assert_eq!(decide(three, open, lanes(2)), Decision::HandOver);
        let higher = Seen {
            tickets: vec![ticket(1, ME, true), ticket(0, OTHER, false)],
            ..seen(1, 0.5)
        };
        assert_eq!(decide(higher, open, lanes(2)), Decision::Keep);
        let alone = Seen {
            tickets: vec![
                ticket(1, ME, true),
                Ticket {
                    alone: true,
                    ..ticket(0, OTHER, false)
                },
            ],
            ..seen(1, 0.5)
        };
        assert_eq!(decide(alone, open, lanes(2)), Decision::HandOver);
        let bench = Seen {
            tickets: vec![
                Ticket {
                    alone: true,
                    ..ticket(3, ME, true)
                },
                ticket(0, OTHER, false),
            ],
            ..seen(3, 60.0)
        };
        assert_eq!(
            decide(bench, open, |_| panic!("ordered while alone")),
            Decision::Keep
        );
    }

    fn open(_: u32) -> bool {
        true
    }

    #[test]
    fn the_slice_keeps_the_gpu_against_its_own_class_for_3_s() {
        assert_eq!(decide(seen(1, 0.0), open, other_first), Decision::Keep);
        assert_eq!(decide(seen(1, 2.9), open, other_first), Decision::Keep);
        assert_eq!(decide(seen(1, 3.0), open, other_first), Decision::HandOver);
        assert_eq!(decide(seen(2, 2.0), open, other_first), Decision::Keep);
        assert_eq!(decide(seen(2, 9.0), open, by_class), Decision::Keep);
        let not_holding = Seen {
            slice_age: None,
            ..seen(1, 0.0)
        };
        assert_eq!(decide(not_holding, open, other_first), Decision::HandOver);
    }

    #[test]
    fn a_waiting_higher_class_gets_in_inside_the_slice() {
        assert_eq!(decide(seen(0, 0.0), open, by_class), Decision::HandOver);
        assert_eq!(decide(seen(0, 2.0), open, by_class), Decision::HandOver);
    }

    #[test]
    fn an_aged_truth_or_search_job_turns_as_clip_held_or_waiting() {
        let with = |mine: Ticket, other: Ticket, slice_s: f64| Seen {
            tickets: vec![mine, other],
            ..seen(1, slice_s)
        };
        let aged = |class, pid, started| Ticket {
            mark: crate::queue::Mark::Aged,
            ..ticket(class, pid, started)
        };
        let clip_holder = || ticket(1, ME, true);
        let aged_bake = || aged(2, OTHER, false);
        assert_eq!(
            decide(with(clip_holder(), aged_bake(), 1.0), open, by_class),
            Decision::Keep
        );
        assert_eq!(
            decide(with(clip_holder(), aged_bake(), 3.0), open, other_first),
            Decision::HandOver
        );
        assert_eq!(
            decide(
                with(ticket(2, ME, true), aged(3, OTHER, false), 0.0),
                open,
                by_class
            ),
            Decision::HandOver
        );
        assert_eq!(
            decide(
                with(ticket(2, ME, true), ticket(3, OTHER, false), 0.0),
                open,
                by_class
            ),
            Decision::Keep
        );
        let aged_holder = || aged(2, ME, true);
        assert_eq!(
            decide(
                with(aged_holder(), ticket(1, OTHER, false), 2.0),
                open,
                by_class
            ),
            Decision::Keep
        );
        assert_eq!(
            decide(
                with(aged_holder(), ticket(1, OTHER, false), 3.0),
                open,
                other_first
            ),
            Decision::HandOver
        );
        assert_eq!(
            decide(
                with(aged_holder(), ticket(0, OTHER, false), 0.0),
                open,
                by_class
            ),
            Decision::HandOver
        );
    }

    #[test]
    fn a_lent_ticket_waits() {
        let lent = Seen {
            lent: true,
            ..seen(1, 1.0)
        };
        assert_eq!(
            decide(lent, open, |_| panic!("ordered while lent")),
            Decision::Lent
        );
    }

    #[test]
    fn a_closed_or_paused_ticket_is_never_handed_to() {
        assert_eq!(
            decide(seen(0, 9.0), |pid| pid != OTHER, by_class),
            Decision::Keep
        );
        let paused = Seen {
            paused: true,
            ..seen(0, 9.0)
        };
        assert_eq!(decide(paused, open, by_class), Decision::Keep);
        assert_eq!(
            decide(
                seen(0, 9.0),
                |pid| pid != ME,
                |_| panic!("ordered without me")
            ),
            Decision::Keep
        );
    }

    #[test]
    fn nobody_first_or_me_first_keeps_the_gpu() {
        assert_eq!(decide(seen(1, 9.0), open, |_| true), Decision::Keep);
    }
}
