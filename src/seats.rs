use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::fake::fake;
use crate::paths::{alive_as, millis, write_atomic};
use crate::queue::{Ticket, holder_of};

pub(crate) const MOST: usize = 4;

pub(crate) fn count() -> usize {
    fake("seats")
        .and_then(|v| v.trim().parse().ok())
        .or_else(crate::config::slots)
        .unwrap_or(1)
        .clamp(1, MOST)
}

pub(crate) fn room(floor: usize, held: usize) -> usize {
    floor.max(held).min(MOST)
}

pub(crate) fn hold_older(dir: &Path) {
    let _ = write_atomic(&dir.join("load"), &format!("{} {}\n", count(), millis()));
}

pub(crate) fn lock_path(dir: &Path, seat: usize) -> PathBuf {
    match seat {
        0 => dir.join("lock"),
        _ => dir.join(format!("lock.{seat}")),
    }
}

pub(crate) fn running_path(dir: &Path, seat: usize) -> PathBuf {
    match seat {
        0 => dir.join("running"),
        _ => dir.join(format!("running.{seat}")),
    }
}

pub(crate) struct Seats {
    files: Vec<File>,
    held: Vec<usize>,
}

impl Seats {
    pub(crate) fn open(dir: &Path) -> io::Result<Self> {
        let files = (0..MOST)
            .map(|seat| {
                OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(lock_path(dir, seat))
            })
            .collect::<io::Result<Vec<File>>>()?;
        Ok(Self {
            files,
            held: Vec::new(),
        })
    }

    pub(crate) fn take(&mut self, alone: bool) -> bool {
        for (seat, file) in self.files.iter().enumerate() {
            if file.try_lock().is_ok() {
                self.held.push(seat);
                if !alone {
                    return true;
                }
            } else if alone {
                self.release();
                return false;
            }
        }
        !self.held.is_empty()
    }

    pub(crate) fn release(&mut self) {
        for seat in self.held.drain(..) {
            let _ = self.files[seat].unlock();
        }
    }

    pub(crate) fn running(&self, dir: &Path) -> PathBuf {
        running_path(dir, self.held.first().copied().unwrap_or(0))
    }
}

pub(crate) struct Holder {
    pub(crate) label: String,
    pub(crate) pid: u32,
    pub(crate) since: Option<u128>,
}

pub(crate) fn holders(dir: &Path) -> Vec<Holder> {
    (0..MOST)
        .filter_map(|seat| {
            let path = running_path(dir, seat);
            let body = fs::read_to_string(&path).ok()?;
            let (pid, start) = holder_of(&body)?;
            if !alive_as(pid, start) {
                return None;
            }
            let mut lines = body.lines().skip(1);
            Some(Holder {
                label: lines.next()?.to_string(),
                pid,
                since: fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis()),
            })
        })
        .collect()
}

pub(crate) fn holding(dir: &Path) -> Vec<u32> {
    holders(dir).iter().map(|h| h.pid).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    Alone(u32),
    Empty,
    Clear,
    Seats,
}

pub(crate) struct Line<'a> {
    pub(crate) open: &'a [&'a Ticket],
    pub(crate) holding: &'a [u32],
    pub(crate) room: usize,
}

impl Line<'_> {
    pub(crate) fn within(&self, me: u32) -> Result<(), Wait> {
        let Some(at) = self.open.iter().position(|t| t.pid == me) else {
            return Err(Wait::Seats);
        };
        if let Some(ahead) = self.open[..at].iter().find(|t| t.alone) {
            return Err(Wait::Alone(ahead.pid));
        }
        if self.open[at].alone {
            return if at == 0 { Ok(()) } else { Err(Wait::Clear) };
        }
        if at >= self.room {
            return Err(Wait::Seats);
        }
        Ok(())
    }

    pub(crate) fn admits(&self, me: u32, list: &[Ticket]) -> Result<(), Wait> {
        if let Some(alone) = list
            .iter()
            .find(|t| t.alone && t.pid != me && self.holding.contains(&t.pid))
        {
            return Err(Wait::Alone(alone.pid));
        }
        self.within(me)?;
        let others = self.holding.iter().filter(|pid| **pid != me).count();
        let alone = self.open.iter().any(|t| t.pid == me && t.alone);
        if alone && others > 0 {
            return Err(Wait::Clear);
        }
        if others >= self.room {
            return Err(Wait::Seats);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Line, Seats, Wait, lock_path, running_path};

    use crate::queue::Ticket;
    use std::fs;
    use std::path::PathBuf;

    fn ticket(pid: u32, alone: bool) -> Ticket {
        Ticket {
            alone,
            ..Ticket::bare(1, pid, "a")
        }
    }

    fn line<'a>(open: &'a [&'a Ticket], holding: &'a [u32], room: usize) -> Line<'a> {
        Line {
            open,
            holding,
            room,
        }
    }

    #[test]
    fn seat_zero_keeps_the_old_names_so_an_older_wrapper_shares_it() {
        let dir = PathBuf::from("/q");
        assert_eq!(lock_path(&dir, 0), dir.join("lock"));
        assert_eq!(running_path(&dir, 0), dir.join("running"));
        assert_eq!(lock_path(&dir, 1), dir.join("lock.1"));
        assert_eq!(running_path(&dir, 1), dir.join("running.1"));
    }

    #[test]
    fn two_jobs_run_when_there_is_room_and_the_third_waits() {
        let (a, b, c) = (ticket(1, false), ticket(2, false), ticket(3, false));
        let open = [&a, &b, &c];
        assert_eq!(line(&open, &[1], 2).admits(2, &[]), Ok(()));
        assert_eq!(line(&open, &[1, 2], 2).admits(3, &[]), Err(Wait::Seats));
        assert_eq!(line(&open, &[1], 2).admits(3, &[]), Err(Wait::Seats));
        assert_eq!(line(&open, &[1], 1).admits(2, &[]), Err(Wait::Seats));
        assert_eq!(line(&open, &[], 1).admits(1, &[]), Ok(()));
        assert_eq!(line(&open, &[2], 2).within(1), Ok(()));
        assert_eq!(line(&open, &[1, 2], 2).within(3), Err(Wait::Seats));
    }

    #[test]
    fn a_job_that_runs_alone_waits_for_the_gpu_to_clear_and_keeps_everyone_behind_it_out() {
        let (a, e, c) = (ticket(1, false), ticket(2, true), ticket(3, false));
        let open = [&a, &e, &c];
        let list = [ticket(1, false), ticket(2, true), ticket(3, false)];
        assert_eq!(line(&open, &[], 2).admits(1, &list), Ok(()));
        assert_eq!(line(&open, &[1], 2).admits(2, &list), Err(Wait::Clear));
        assert_eq!(line(&open, &[1], 2).admits(3, &list), Err(Wait::Alone(2)));
        let first = [&e, &a, &c];
        assert_eq!(line(&first, &[1], 2).admits(2, &list), Err(Wait::Clear));
        assert_eq!(line(&first, &[], 2).admits(2, &list), Ok(()));
        assert_eq!(line(&first, &[1], 2).within(1), Err(Wait::Alone(2)));
        let later = [&a, &c, &e];
        assert_eq!(line(&later, &[2], 2).admits(1, &list), Err(Wait::Alone(2)));
    }

    #[test]
    fn a_seat_is_one_lock_and_a_job_alone_takes_every_seat_or_none() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-seats");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let open = |n: usize| Seats {
            files: (0..n)
                .map(|seat| {
                    fs::OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .write(true)
                        .open(lock_path(&dir, seat))
                        .unwrap()
                })
                .collect(),
            held: Vec::new(),
        };
        let (mut a, mut b, mut c) = (open(2), open(2), open(2));
        assert!(a.take(false));
        assert!(!c.take(true), "one seat is taken");
        assert!(b.take(false));
        assert_eq!(b.running(&dir), running_path(&dir, 1));
        assert!(!c.take(false));
        a.release();
        b.release();
        assert!(c.take(true));
        assert_eq!(c.held, vec![0, 1]);
        assert!(!a.take(false));
    }
}
