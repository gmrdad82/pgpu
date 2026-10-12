use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::paths::{home, make_home, say};
use crate::procs::{rescue, turned_this_second};
use crate::queue::NAMES;
use crate::queue::{clean, flag};
use crate::report::{stats, status};
use crate::run::{Ask, run};
use crate::turn::turn;

const RULES: &str = "\
Jobs run GPU_SLOTS at once (~/.config/pgpu/config, else one), interactive first, sessions
taking turns within a class. The lanes follow the card: one more opens, up to 4, while jobs wait
and the card stays under 70% busy over 10 s with VRAM under 80%.
Running alone is only for re-running a number that looks skewed, with --alone \"<why>\": it waits
for an empty queue, or at night (01:00 to 07:00) for the running jobs to end with nothing new
starting, then keeps every lane until it ends. A truth or search job whose waits add up to 10
minutes takes turns as a clip, and so does an interactive job that has run 60 s or never calls
turn in its first 5 s. A running job calls `pgpu turn` between passes to let a waiting job in.
Classes and their budgets: interactive 5m, test 10m, clip and render 25m, bench 20m, truth and
search 1h (--budget sets another). Past its budget a job shows red in status; at twice its
budget, or after 5 minutes with no GPU work, turn or CPU, pgpu stops it (SIGTERM, SIGKILL after
15 s) and exits 124. Every job has a memory ceiling, 24G (--memory sets another); it
slows at 15/16 of it and the kernel kills the whole job past it, with no swap. A job frozen or
waiting for the GPU a second or more after it started says so on stderr when it ends, since its
own wall-clock numbers are skewed; the file GPU_QUEUE_FROZEN names holds that time in ms while
it runs. With no --class, `pgpu run` takes its class from PGPU_CLASS, else interactive.";

const TURN: &str = "\
{about-with-newline}
{usage-heading} {usage}

Options:
      --work-ms <MS>     GPU time the job's own submissions took since its previous turn
      --longest-ms <MS>  Its longest single submission since then
  -h, --help             Print help

Unknown or malformed arguments are ignored: turn never fails a job.
";

#[derive(Parser)]
#[command(
    name = "pgpu",
    version,
    about = "share one GPU between jobs and agents",
    after_help = RULES,
    arg_required_else_help = false
)]
struct Cli {
    #[command(subcommand)]
    command: Call,
}

#[derive(Subcommand)]
enum Call {
    #[command(about = "Run a GPU job in the queue", args_override_self = true)]
    Run {
        #[arg(
            long,
            value_name = "CLASS",
            value_parser = NAMES,
            help = "Priority and budget, else PGPU_CLASS, else interactive"
        )]
        class: Option<String>,
        #[arg(
            long = "as",
            value_name = "NAME",
            help = "Session name, else the nearest folder holding .git"
        )]
        who: Option<String>,
        #[arg(
            long,
            value_name = "DURATION",
            value_parser = humantime::parse_duration,
            help = "Run time before it turns red, stopped at twice that (90s, 15m, 1h30m), else its class's"
        )]
        budget: Option<std::time::Duration>,
        #[arg(
            long,
            value_name = "SIZE",
            value_parser = crate::budget::parse_memory,
            help = "Memory ceiling, killed past it (32G, 48G), else its class's 24G"
        )]
        memory: Option<u64>,
        #[arg(
            long,
            value_name = "REASON",
            help = "Re-run alone a number that looks skewed, saying why; it waits for an empty queue or the night (01:00 to 07:00), never jumps the line or freezes a job, and no other job runs beside it"
        )]
        alone: Option<String>,
        #[arg(long, hide = true)]
        exclusive: bool,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    #[command(
        about = "Let a waiting job in between passes, and report the pacing",
        override_usage = "pgpu turn [--work-ms MS] [--longest-ms MS]",
        help_template = TURN
    )]
    Turn {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        args: Vec<OsString>,
    },
    #[command(about = "What holds the GPU, what waits and why")]
    Status {
        #[arg(long, help = "One JSON object (docs/json.md)")]
        json: bool,
    },
    #[command(
        about = "How the GPU was shared, the jobs pgpu stopped, and each job's pacing",
        args_override_self = true
    )]
    Stats {
        #[arg(long, value_name = "N", default_value_t = 7, help = "Days back")]
        days: u64,
        #[arg(long, help = "Each session's pacing")]
        pace: bool,
        #[arg(long, help = "JSON (docs/json.md)")]
        json: bool,
    },
    #[command(about = "Hold the queue")]
    Pause,
    #[command(about = "Release the queue")]
    Resume,
}

pub(crate) fn main() -> ExitCode {
    dispatch(std::env::args_os().skip(1).collect())
}

fn dispatch(args: Vec<OsString>) -> ExitCode {
    let call = match Cli::try_parse_from(std::iter::once(OsString::from("pgpu")).chain(args)) {
        Ok(cli) => cli.command,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(e.exit_code() as u8);
        }
    };
    if let Call::Run {
        alone, exclusive, ..
    } = &call
        && (*exclusive || alone.as_deref().is_some_and(|why| why.trim().is_empty()))
    {
        say(
            "pgpu: running alone is only for re-running a number that looks skewed: --alone \"<why it looks skewed>\"; without it every job, bench included, shares the lanes",
        );
        return ExitCode::from(1);
    }
    let dir = if matches!(call, Call::Run { .. } | Call::Pause | Call::Resume) {
        make_home()
    } else {
        home()
    };
    if !(matches!(call, Call::Turn { .. }) && turned_this_second(&dir)) {
        rescue(&dir, true);
    }
    match call {
        Call::Run {
            class,
            who,
            budget,
            memory,
            alone,
            exclusive: _,
            command,
        } => run(Ask {
            class: class.as_deref(),
            who,
            budget,
            memory,
            alone: alone
                .map(|why| clean(why.trim()))
                .filter(|why| !why.is_empty()),
            command: &command,
        }),
        Call::Status { json } => status(json),
        Call::Turn { args } => {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            turn(&args)
        }
        Call::Stats { days, pace, json } => stats(days.into(), pace, json),
        Call::Pause => flag(true),
        Call::Resume => flag(false),
    }
}

#[cfg(test)]
mod tests {
    use super::{Call, Cli, dispatch};
    use clap::Parser;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, ExitCode};

    const CALL: &str = "PGPU_UNIT_CALL";

    fn missing(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-cli-{name}"));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn call_in_child(name: &str, call: &str, extra: &[(&str, &str)]) -> PathBuf {
        let root = missing(name);
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "cli::tests::the_call_in_the_child",
                "--nocapture",
            ])
            .env(CALL, call)
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("HOME", root.join("home"))
            .env("PGPU_FAKE", root.join("fake"));
        for (key, value) in extra {
            child.env(key, value);
        }
        let out = child.output().unwrap();
        assert!(
            out.status.success(),
            "{call}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        root
    }

    fn parsed(words: &[&str]) -> Call {
        Cli::try_parse_from(std::iter::once("pgpu").chain(words.iter().copied()))
            .unwrap()
            .command
    }

    #[test]
    fn turn_takes_any_words_and_run_keeps_its_command_whole() {
        let words = [
            "--work-ms",
            "12.5",
            "--bogus",
            "-z",
            "--longest-ms=3",
            "nope",
        ];
        let Call::Turn { args } = parsed(&[&["turn"], &words[..]].concat()) else {
            panic!("turn");
        };
        assert_eq!(args, words.map(OsString::from));
        let Call::Run {
            class,
            who,
            budget,
            memory,
            alone,
            exclusive,
            command,
        } = parsed(&[
            "run",
            "--class",
            "clip",
            "--as",
            "pfx",
            "--budget",
            "1h30m",
            "--memory",
            "48G",
            "--alone",
            "4K time 9% off its baseline",
            "--",
            "cmd",
            "--flag",
            "--",
            "-x",
        ])
        else {
            panic!("run");
        };
        assert_eq!(class.as_deref(), Some("clip"));
        assert_eq!(who.as_deref(), Some("pfx"));
        assert_eq!(budget, Some(std::time::Duration::from_secs(5400)));
        assert_eq!(memory, Some(48 << 30));
        assert_eq!(alone.as_deref(), Some("4K time 9% off its baseline"));
        assert!(!exclusive);
        assert_eq!(command, ["cmd", "--flag", "--", "-x"]);
    }

    #[test]
    fn the_call_in_the_child() {
        let Ok(call) = std::env::var(CALL) else {
            return;
        };
        let args = call.split(' ').map(OsString::from).collect();
        assert_eq!(
            format!("{:?}", dispatch(args)),
            format!("{:?}", ExitCode::SUCCESS)
        );
    }

    #[test]
    fn status_stats_and_a_turn_work_with_the_queue_folders_missing() {
        for (name, call) in [
            ("status", "status"),
            ("json", "status --json"),
            ("stats", "stats"),
            ("pace", "stats --pace --json"),
        ] {
            let root = call_in_child(name, call, &[]);
            assert!(!root.join("run/pito-gpu-queue").exists(), "{call}");
        }
        let root = missing("turn");
        let turn = root.join("run/pito-gpu-queue/turns/4242");
        let root = call_in_child(
            "turn",
            "turn",
            &[
                ("GPU_QUEUE_TURN", turn.to_str().unwrap()),
                ("GPU_QUEUE_PID", "4242"),
            ],
        );
        assert!(!root.join("run/pito-gpu-queue").exists());
    }

    #[test]
    fn pause_and_resume_create_what_they_write() {
        let root = call_in_child("pause", "pause", &[]);
        assert!(root.join("run/pito-gpu-queue/paused").exists());
        let root = call_in_child("resume", "resume", &[]);
        assert!(!root.join("run/pito-gpu-queue/paused").exists());
    }
}
