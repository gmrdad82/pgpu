use std::time::Duration;

use crate::fake::fake;

pub(crate) const STALL: Duration = Duration::from_secs(300);

pub(crate) const GRACE: Duration = Duration::from_secs(15);

pub(crate) const LONG: Duration = Duration::from_secs(60);

pub(crate) const UNPACED: Duration = Duration::from_secs(5);

pub(crate) const STOPPED: u8 = 124;

pub(crate) const GIB: u64 = 1 << 30;

pub(crate) const MEMORY: u64 = 24 * GIB;

const CPU_BUSY: f64 = 0.05;

fn faked(name: &str, real: Duration) -> Duration {
    fake(name)
        .and_then(|ms| ms.parse().ok())
        .map_or(real, Duration::from_millis)
}

pub(crate) fn stall() -> Duration {
    faked("stall_ms", STALL)
}

pub(crate) fn grace() -> Duration {
    faked("grace_ms", GRACE)
}

pub(crate) fn high(max: u64) -> u64 {
    max - max / 16
}

pub(crate) fn parse_memory(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let shift = match unit.to_ascii_lowercase().as_str() {
        "k" | "kb" | "kib" => 10,
        "m" | "mb" | "mib" => 20,
        "g" | "gb" | "gib" | "" => 30,
        "t" | "tb" | "tib" => 40,
        _ => return Err(format!("unknown unit '{unit}' (use M, G or T)")),
    };
    let bytes = number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_shl(shift).filter(|b| b >> shift == n))
        .ok_or_else(|| format!("not a size: '{text}' (24G, 48G, 512M)"))?;
    if bytes < 1 << 20 {
        return Err("under 1M".to_string());
    }
    Ok(bytes)
}

pub(crate) fn size(bytes: u64) -> String {
    if bytes.is_multiple_of(GIB) {
        format!("{}G", bytes / GIB)
    } else if bytes >= GIB {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    } else {
        format!("{}M", bytes >> 20)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Budget,
    Stall,
}

impl Stop {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Stop::Budget => "budget",
            Stop::Stall => "stall",
        }
    }
}

pub(crate) fn span(time: Duration) -> String {
    let s = time.as_secs();
    if s >= 3600 && s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 && s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub(crate) fn cpu_busy(cpu: Duration, wall: Duration) -> bool {
    !wall.is_zero() && cpu.as_secs_f64() >= wall.as_secs_f64() * CPU_BUSY
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    pub(crate) budget: Duration,
    stall: Duration,
    pub(crate) ran: Duration,
    quiet: Duration,
    told: bool,
}

impl Clock {
    pub(crate) fn new(budget: Duration, stall: Duration) -> Self {
        Self {
            budget,
            stall,
            ran: Duration::ZERO,
            quiet: Duration::ZERO,
            told: false,
        }
    }

    pub(crate) fn tick(&mut self, time: Duration, running: bool) {
        if running {
            self.ran += time;
            self.quiet += time;
        }
    }

    pub(crate) fn busy(&mut self) {
        self.quiet = Duration::ZERO;
    }

    pub(crate) fn quiet(&self) -> Duration {
        self.quiet
    }

    pub(crate) fn over(&self) -> Option<Duration> {
        self.ran
            .checked_sub(self.budget)
            .filter(|over| !over.is_zero())
    }

    pub(crate) fn passed(&mut self) -> bool {
        let first = !self.told && self.over().is_some();
        self.told |= first;
        first
    }

    pub(crate) fn verdict(&self) -> Option<Stop> {
        if self.ran >= self.budget.saturating_mul(2) {
            Some(Stop::Budget)
        } else if self.quiet >= self.stall {
            Some(Stop::Stall)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, GIB, Stop, cpu_busy, high, parse_memory, size, span};
    use std::time::Duration;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_job_turns_red_past_its_budget_and_is_stopped_at_twice_it() {
        let mut clock = Clock::new(10 * S, 100 * S);
        clock.tick(10 * S, true);
        clock.busy();
        assert_eq!(clock.over(), None);
        assert!(!clock.passed());
        clock.tick(3 * S, true);
        clock.busy();
        assert_eq!(clock.over(), Some(3 * S));
        assert!(clock.passed());
        assert!(!clock.passed(), "said once");
        assert_eq!(clock.verdict(), None);
        clock.tick(60 * S, false);
        assert_eq!(
            clock.verdict(),
            None,
            "time frozen or waiting is not run time"
        );
        clock.tick(7 * S, true);
        assert_eq!(clock.verdict(), Some(Stop::Budget));
    }

    #[test]
    fn a_stall_is_run_time_with_no_gpu_work_turn_or_cpu_and_any_of_them_resets_it() {
        let mut clock = Clock::new(1000 * S, 300 * S);
        clock.tick(299 * S, true);
        assert_eq!(clock.verdict(), None);
        clock.busy();
        clock.tick(299 * S, true);
        clock.tick(600 * S, false);
        assert_eq!(clock.verdict(), None, "waiting for the GPU is no stall");
        clock.tick(S, true);
        assert_eq!(clock.verdict(), Some(Stop::Stall));
        assert!(cpu_busy(Duration::from_millis(60), S));
        assert!(!cpu_busy(Duration::from_millis(40), S));
        assert!(!cpu_busy(S, Duration::ZERO));
    }

    #[test]
    fn memory_sizes_read_as_written_and_the_soft_limit_sits_below_the_ceiling() {
        assert_eq!(parse_memory("24G"), Ok(24 * GIB));
        assert_eq!(parse_memory("48GiB"), Ok(48 * GIB));
        assert_eq!(parse_memory("32"), Ok(32 * GIB));
        assert_eq!(parse_memory("512M"), Ok(512 << 20));
        assert_eq!(parse_memory("1t"), Ok(1 << 40));
        for bad in ["", "G", "24X", "-1G", "1.5G", "0", "512K", "99999999999T"] {
            assert!(parse_memory(bad).is_err(), "{bad}");
        }
        assert_eq!(high(24 * GIB), 24 * GIB - 1536 * (1 << 20));
        assert_eq!(size(24 * GIB), "24G");
        assert_eq!(size(GIB + GIB / 2), "1.5G");
        assert_eq!(size(64 << 20), "64M");
    }

    #[test]
    fn spans_read_as_written() {
        assert_eq!(span(Duration::from_secs(300)), "5m");
        assert_eq!(span(Duration::from_secs(3600)), "1h");
        assert_eq!(span(Duration::from_secs(3725)), "1h02m");
        assert_eq!(span(Duration::from_secs(95)), "1m35s");
        assert_eq!(span(Duration::from_secs(42)), "42s");
    }
}
