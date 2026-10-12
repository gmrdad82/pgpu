use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use crate::budget::{STOPPED, Stop};

const LOOK_EVERY: Duration = Duration::from_secs(1);

const LOOK_LENT: Duration = Duration::from_millis(100);

const LEND_UNDER: f64 = 0.01;

const LEND_AFTER: Duration = Duration::from_secs(3);

const TAKE_BACK: f64 = 0.05;

pub(crate) const IDLE: f64 = 0.05;

pub(crate) const BENCH_LEND_AFTER: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leave {
    Signal(i32),
    Done(Option<ExitStatus>),
    Stopped(Stop),
}

impl Leave {
    pub(crate) fn code(&self) -> u8 {
        match self {
            Leave::Signal(sig) => signal_code(*sig),
            Leave::Done(status) => exit_code(*status),
            Leave::Stopped(_) => STOPPED,
        }
    }
}

pub(crate) fn signal_code(sig: i32) -> u8 {
    (128 + sig).clamp(0, 255) as u8
}

fn exit_code(status: Option<ExitStatus>) -> u8 {
    match status.map(|s| (s.code(), s.signal())) {
        Some((Some(code), _)) => code.clamp(0, 255) as u8,
        Some((None, Some(sig))) => signal_code(sig),
        _ => 1,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Pace {
    pub(crate) job_gpu: Option<f64>,
    pub(crate) video: bool,
    pub(crate) kfd: bool,
}

impl Pace {
    pub(crate) fn idle(&self, under: f64) -> bool {
        self.job_gpu.is_some_and(|share| share < under) && !self.video && !self.kfd
    }

    fn wants_gpu(&self) -> bool {
        self.job_gpu.is_some_and(|share| share >= TAKE_BACK) || self.video || self.kfd
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Look {
    pub(crate) lend: bool,
    pub(crate) pace: Pace,
    pub(crate) next_up: bool,
    pub(crate) yields: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) now: Instant,
    pub(crate) leave: Option<Leave>,
    pub(crate) look: Option<Look>,
    pub(crate) turned_ago: Option<Duration>,
    pub(crate) turn_wait: bool,
    pub(crate) regained: bool,
}

impl Seen {
    pub(crate) fn leaving(now: Instant, leave: Leave) -> Self {
        Self {
            now,
            leave: Some(leave),
            look: None,
            turned_ago: None,
            turn_wait: false,
            regained: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Running,
    Parked,
    Lent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    Park,
    Resume,
    HandOver,
    Lend,
    TakeBack,
    Reclaim,
    Yield,
    Exit(Leave),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Wrapper {
    mode: Mode,
    next_look: Instant,
    held_since: Instant,
    quiet_since: Option<Instant>,
    alone: bool,
}

impl Wrapper {
    pub(crate) fn new(now: Instant, alone: bool) -> Self {
        Self {
            mode: Mode::Running,
            next_look: now,
            held_since: now,
            quiet_since: None,
            alone,
        }
    }

    pub(crate) fn runs(&self) -> bool {
        self.mode != Mode::Parked
    }

    pub(crate) fn looks_due(&self, now: Instant) -> bool {
        now >= self.next_look
    }

    fn lends(&self, seen: &Seen, look: &Look) -> bool {
        let now = seen.now;
        let after = if self.alone {
            BENCH_LEND_AFTER
        } else {
            LEND_AFTER
        };
        look.lend
            && look.next_up
            && seen.turned_ago.is_none_or(|ago| ago >= after)
            && !seen.turn_wait
            && self.mode == Mode::Running
            && self
                .quiet_since
                .is_some_and(|since| now.saturating_duration_since(since) >= after)
            && now.saturating_duration_since(self.held_since) >= after
    }

    pub(crate) fn decide(mut self, seen: &Seen) -> (Self, Vec<Act>) {
        if let Some(leave) = seen.leave {
            return (self, vec![Act::Exit(leave)]);
        }
        if seen.regained {
            self.held_since = seen.now;
            self.quiet_since = None;
        }
        let lent = self.mode == Mode::Lent;
        if let Some(look) = &seen.look {
            let under = if self.alone { IDLE } else { LEND_UNDER };
            self.quiet_since =
                (!lent && look.pace.idle(under)).then(|| self.quiet_since.unwrap_or(seen.now));
            self.next_look = seen.now
                + if lent && self.alone {
                    LOOK_LENT
                } else {
                    LOOK_EVERY
                };
        }
        if lent {
            if seen.turn_wait || seen.look.as_ref().is_some_and(|look| look.pace.wants_gpu()) {
                self.next_look = seen.now;
                if self.alone {
                    self.mode = Mode::Running;
                    return (self, vec![Act::Reclaim]);
                }
                self.mode = Mode::Parked;
                return (self, vec![Act::Park, Act::TakeBack]);
            }
            return (self, Vec::new());
        }
        if self.mode == Mode::Running
            && !self.alone
            && seen.look.as_ref().is_some_and(|look| look.yields)
        {
            self.mode = Mode::Parked;
            self.quiet_since = None;
            return (self, vec![Act::Park, Act::Yield]);
        }
        if seen
            .look
            .as_ref()
            .is_some_and(|look| self.lends(seen, look))
        {
            self.mode = Mode::Lent;
            self.quiet_since = None;
            return (self, vec![Act::Lend]);
        }
        let mut acts = Vec::new();
        if self.mode == Mode::Parked {
            acts.push(Act::Resume);
            self.mode = Mode::Running;
        }
        if seen.turn_wait {
            acts.push(Act::HandOver);
        }
        (self, acts)
    }

    pub(crate) fn finish(self) -> Vec<Act> {
        if self.mode == Mode::Parked {
            vec![Act::Resume]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Act, Leave, Look, Mode, Pace, Seen, Wrapper};
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::time::{Duration, Instant};

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn look(job_gpu: f64, next_up: bool) -> Look {
        Look {
            lend: true,
            pace: Pace {
                job_gpu: Some(job_gpu),
                ..Pace::default()
            },
            next_up,
            yields: false,
        }
    }

    fn seen(now: Instant, look: Option<Look>) -> Seen {
        Seen {
            now,
            leave: None,
            look,
            turned_ago: None,
            turn_wait: false,
            regained: false,
        }
    }

    fn looks(mut wrapper: Wrapper, from: Instant, secs: u64, at: Look) -> (Wrapper, Vec<Act>) {
        let mut acts = Vec::new();
        for n in 0..=secs {
            let (next, got) = wrapper.decide(&seen(from + s(n), Some(at)));
            wrapper = next;
            acts.extend(got);
        }
        (wrapper, acts)
    }

    #[test]
    fn a_job_ended_by_a_signal_exits_128_plus_it_and_a_stopped_one_124() {
        assert_eq!(Leave::Signal(15).code(), 143);
        assert_eq!(Leave::Done(Some(ExitStatus::from_raw(3 << 8))).code(), 3);
        assert_eq!(Leave::Done(Some(ExitStatus::from_raw(9))).code(), 137);
        assert_eq!(Leave::Done(None).code(), 1);
        assert_eq!(Leave::Stopped(crate::budget::Stop::Stall).code(), 124);
        let now = Instant::now();
        let (_, acts) = Wrapper::new(now, false).decide(&Seen::leaving(now, Leave::Signal(2)));
        assert_eq!(acts, vec![Act::Exit(Leave::Signal(2))]);
    }

    #[test]
    fn a_holder_idle_for_three_seconds_lends_the_gpu_and_never_before() {
        let now = Instant::now();
        let (_, acts) = looks(Wrapper::new(now, false), now, 2, look(0.0, true));
        assert!(acts.is_empty());
        let (lent, acts) = looks(Wrapper::new(now, false), now, 3, look(0.0, true));
        assert_eq!(acts, vec![Act::Lend]);
        assert_eq!(lent.mode, Mode::Lent);
        let (_, acts) = looks(Wrapper::new(now, false), now, 9, look(0.0, false));
        assert!(acts.is_empty(), "no one to start");
        let (_, acts) = looks(Wrapper::new(now, false), now, 9, look(0.5, true));
        assert!(acts.is_empty(), "it uses the GPU");
        let off = Look {
            lend: false,
            ..look(0.0, true)
        };
        let (_, acts) = looks(Wrapper::new(now, false), now, 9, off);
        assert!(acts.is_empty(), "lend is off");
    }

    #[test]
    fn a_lent_job_takes_the_gpu_back_frozen_at_five_percent_or_a_turn_and_thaws_once_it_holds() {
        let now = Instant::now();
        let (lent, _) = looks(Wrapper::new(now, false), now, 3, look(0.0, true));
        let (_, acts) = lent.decide(&seen(now + s(4), Some(look(0.04, true))));
        assert!(acts.is_empty());
        let (back, acts) = lent.decide(&seen(now + s(4), Some(look(0.05, true))));
        assert_eq!(acts, vec![Act::Park, Act::TakeBack]);
        assert!(!back.runs());
        let turned = Seen {
            turn_wait: true,
            ..seen(now + s(4), None)
        };
        assert_eq!(lent.decide(&turned).1, vec![Act::Park, Act::TakeBack]);
        let regained = Seen {
            regained: true,
            ..seen(now + s(5), None)
        };
        let (holder, acts) = back.decide(&regained);
        assert_eq!(acts, vec![Act::Resume]);
        assert!(holder.runs());
        let (_, acts) = looks(holder, now + s(6), 1, look(0.0, true));
        assert!(acts.is_empty(), "its three seconds count from its return");
        assert_eq!(back.finish(), vec![Act::Resume]);
    }

    #[test]
    fn a_bench_lends_after_ten_seconds_under_five_percent_and_takes_back_unfrozen() {
        let now = Instant::now();
        let bench = Wrapper::new(now, true);
        let (_, acts) = looks(bench, now, 9, look(0.04, true));
        assert!(acts.is_empty());
        let (_, acts) = looks(bench, now, 10, look(0.05, true));
        assert!(acts.is_empty(), "five percent is work");
        let (_, acts) = looks(bench, now, 30, look(0.04, false));
        assert!(acts.is_empty(), "no one waits");
        let (lent, acts) = looks(bench, now, 10, look(0.04, true));
        assert_eq!(acts, vec![Act::Lend]);
        let (_, acts) = lent.decide(&seen(now + s(11), Some(look(0.04, true))));
        assert!(acts.is_empty());
        let (back, acts) = lent.decide(&seen(now + s(11), Some(look(0.05, true))));
        assert_eq!(acts, vec![Act::Reclaim]);
        assert!(
            back.runs(),
            "a bench is never frozen to take its lanes back"
        );
        let turned = Seen {
            turn_wait: true,
            ..seen(now + s(11), None)
        };
        assert_eq!(lent.decide(&turned).1, vec![Act::Reclaim]);
    }

    #[test]
    fn a_holder_freezes_and_gives_its_lane_back_when_a_bench_reclaims() {
        let now = Instant::now();
        let reclaimed = Look {
            yields: true,
            ..look(0.9, false)
        };
        let (frozen, acts) = Wrapper::new(now, false).decide(&seen(now, Some(reclaimed)));
        assert_eq!(acts, vec![Act::Park, Act::Yield]);
        assert!(!frozen.runs());
        let regained = Seen {
            regained: true,
            ..seen(now + s(1), None)
        };
        assert_eq!(frozen.decide(&regained).1, vec![Act::Resume]);
        let (_, acts) = Wrapper::new(now, true).decide(&seen(now, Some(reclaimed)));
        assert!(acts.is_empty(), "a bench never yields");
    }

    #[test]
    fn a_turn_that_waits_hands_over() {
        let now = Instant::now();
        let waits = Seen {
            turn_wait: true,
            turned_ago: Some(Duration::ZERO),
            ..seen(now, None)
        };
        assert_eq!(
            Wrapper::new(now, false).decide(&waits).1,
            vec![Act::HandOver]
        );
    }
}
