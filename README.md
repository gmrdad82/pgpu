<p align="center"><img src="docs/images/status.gif" width="800" alt="A terminal: GPU jobs are queued with pgpu run, and pgpu status shows the ones holding the GPU's lanes, how busy the card is, and each waiting job with why it waits; when a running job ends, the next one takes its lane."></p>

# pgpu

[![CI](https://github.com/gmrdad82/pgpu/actions/workflows/ci.yml/badge.svg)](https://github.com/gmrdad82/pgpu/actions/workflows/ci.yml) [![Version](https://img.shields.io/github/v/tag/gmrdad82/pgpu)](https://github.com/gmrdad82/pgpu/tags)

pgpu shares one GPU between jobs on a Linux machine with an AMD GPU, part of [PITO MD](https://pitomd.com). Put `pgpu run --` in front of a GPU command: it waits for a lane, runs, and hands the card on the moment it ends. Renders, tests, benchmarks and searches from many terminals, scripts and agent sessions take turns instead of fighting over the card, and the card stays busy.

- **2.1 ms** to start a job through pgpu, **5.7 ms** to hand a lane to the next job, **1.4 ms** for `pgpu status`.
- **One 2.0 MB binary**, 770 KB to download, no daemon: the queue is a folder of tickets, and every `pgpu` call does its share.
- **5 direct dependencies** (clap, anstream, anstyle, humantime, libc), 10,482 lines of Rust, a clean release build in 11.2 s.
- **3.5 MB** of memory for each job's waiting wrapper.

## In 30 seconds

```
v=$(basename "$(curl -fsSLo /dev/null -w '%{url_effective}' https://github.com/gmrdad82/pgpu/releases/latest)")
mkdir -p ~/.local/bin && curl -fsSL "https://github.com/gmrdad82/pgpu/releases/download/$v/pgpu-$v-linux-x86_64.tar.gz" | tar -xz -C ~/.local/bin pgpu
pgpu run --class render -- blender -b scene.blend -a &
pgpu run --class test -- cargo test &
pgpu status
```

Two lanes? Write `GPU_SLOTS=2` to `~/.config/pgpu/config`. Every other knob is optional ([Configuration](#configuration)).

## Lanes

`GPU_SLOTS` jobs run at once, never more. A free lane goes to the first job in line, and a waiter that doesn't take its lane within 3 s is passed over, so whatever waits fills the lanes. A running job never loses its lane, and nothing runs alone: `--alone` and `--exclusive` are still accepted so older scripts keep working, and only print that the job shares a lane. `pgpu status` shows what holds each lane, how busy the card is, and why each waiting job waits (the GIF at the top).

## Classes and turns

<p align="center"><img src="docs/images/turn.gif" width="800" alt="A terminal with one GPU lane: a render in passes holds it; an interactive preview job queues and pgpu status shows it waiting its turn; at the render's next pgpu turn the preview runs and prints, then the render takes the lane back."></p>

Interactive jobs go first, then tests, clips, renders and benches, then ground-truth renders, then searches. A job that renders in passes calls `pgpu turn` between them, so a waiting job gets in without the render ending: above, a preview waits for the one lane and runs at the render's next turn. Sessions in the same class take turns.

## Budgets and memory ceilings

<p align="center"><img src="docs/images/limits.gif" width="800" alt="A terminal: a job with a 64M memory ceiling that allocates 300 MB is stopped out of memory and exits 137; a job with a 2s budget shows red in pgpu status as over its budget and is stopped at 4s, exit 124."></p>

Every job runs in its own cgroup with a memory ceiling, 24G unless `--memory` says otherwise, so one job can't take the machine down: past it, the kernel stops the job and `pgpu run` exits 137. Every class has a run-time budget. A job past it shows red in `pgpu status`, and pgpu stops it at twice its budget (exit 124), or after 5 minutes with no GPU work, turn or CPU. Time spent waiting or frozen never counts.

## For scripts and agents

<p align="center"><img src="docs/images/json.gif" width="800" alt="A terminal: three jobs are queued on two lanes; pgpu status --json piped through jq shows the card's busy percent and VRAM, then each job's session, class, state and why it waits."></p>

`pgpu status --json` and `pgpu stats --json` are a stable contract ([docs/json.md](docs/json.md)): fields are only ever added. Each exit code means one thing, and a job sees `GPU_QUEUE_FROZEN`, a file with the milliseconds it spent frozen or waiting, so it can correct its own timings.

## Speed

<p align="center"><img src="docs/images/latency-bars.png" width="600" alt="Bar chart, milliseconds, median, lower is better: start a job 2.1, pgpu status 1.4, pgpu status --json 1.8, hand-on 5.7."></p>

| What | Number | How it was measured |
| --- | --- | --- |
| Start a job | 2.1 ms median, 8.8 ms p90 | `pgpu run -- true` (2.92 ms) minus plain `true` (0.81 ms), 100 runs each, empty two-lane sandbox queue |
| Hand a lane on | 5.7 ms median, 13.4 ms p90, 25.5 ms max | one lane, a holder ends and a waiting job starts; time from the holder's last line to the waiter's first, 20 hand-ons |
| `pgpu status` | 1.4 ms median (`--json` 1.8 ms) | 100 calls each, process start included |
| A waiting job's wrapper | 3.5 MB resident | `VmRSS` of 5 waiting wrappers |
| Binary | 2.0 MB, 770 KB release download | v0.31.3 release build and its `linux-x86_64.tar.gz` |
| Clean release build | 11.2 s | `cargo build --release --locked` with an empty target folder, Rust 1.97.1 |
| Code | 10,482 lines, 5 direct dependencies, 19 crates in all | `wc -l src/*.rs`, `cargo tree` |

All measured on 2026-10-14 with pgpu 0.31.3 on a Ryzen 9 9950X and a Radeon RX 9060 XT, once, while the machine's own GPU queue was busy with real jobs. The chart is drawn by [pito-compare](https://github.com/gmrdad82/pito-compare) from [docs/charts/latency.toml](docs/charts/latency.toml).

pgpu is made for Linux with an AMD GPU on the amdgpu driver, and that is all it supports. It reads the card's busy percent and VRAM from amdgpu's sysfs files and each job's GPU use from DRM fdinfo, and it relies on cgroup v2 and `/proc`.

## Install

Each [release](https://github.com/gmrdad82/pgpu/releases/latest) carries a Linux x86_64 binary with a `SHA256SUMS` file; the two lines in [In 30 seconds](#in-30-seconds) install the latest into `~/.local/bin`, which needs to be on your `PATH`. **From source**, with Rust 1.97 or newer:

```
cargo install --locked --git https://github.com/gmrdad82/pgpu --tag vX.Y.Z pgpu
```

Replace `vX.Y.Z` with a [tag](https://github.com/gmrdad82/pgpu/tags).

### What it needs to run

- Linux with cgroup v2 and a systemd user session, so each job gets its own cgroup for its memory ceiling and for freezing. Where pgpu can't make one, it pauses a job with SIGSTOP instead, runs it without a ceiling, and says so.
- An AMD GPU on the amdgpu driver.

## Use

```
pgpu run [--class CLASS] [--as NAME] [--budget DURATION] [--memory SIZE] -- COMMAND [ARGS...]
pgpu turn [--work-ms MS] [--longest-ms MS]
pgpu status [--json]
pgpu stats [--days N] [--pace] [--json]
pgpu pause
pgpu resume
```

Run a job:

```
pgpu run --class render --as my-render -- blender -b scene.blend -a
```

- `--class` is one of `interactive` (5 min budget), `test` (10 min), `clip` or `render` (25 min), `bench` (20 min), `truth` or `search` (1 h). Without it, pgpu takes `PGPU_CLASS`, else `interactive`. An interactive job that runs over 60 s, or never calls `turn` in its first 5 s, takes turns as a clip, and so does a truth or search job whose waits add up to 10 minutes.
- `--as` names the session, so its jobs take turns with other sessions'. Without it, pgpu takes the nearest folder holding `.git`.
- `--budget` sets another run-time budget (`90s`, `15m`, `1h30m`), and `--memory` another ceiling (`32G`, `48G`).

The job sees `GPU_QUEUE=1`, `GPU_QUEUE_TURN`, `GPU_QUEUE_PID` and `GPU_QUEUE_FROZEN` (a file holding the milliseconds it has spent frozen or waiting, so it can correct its own timings). A `pgpu run` inside a job runs its command directly. `pgpu run` exits with its job's code: 124 when pgpu stopped it past twice its budget or stalled, 137 when the kernel killed it at its memory ceiling, and 128 plus the signal when a signal ended it.

A job that renders in passes calls `pgpu turn` between them. Outside a `pgpu run` job it does nothing. Inside one, it returns at once when no waiting job comes first, and otherwise hands the card over and returns when the job's turn comes back. A job that took the card less than 3 s ago keeps it, unless a job of a higher class waits. `--work-ms` (the GPU time the job's own submissions took since its last turn) and `--longest-ms` (its longest single submission) are optional, and `pgpu stats --pace` shows them per session.

`pgpu status` shows what holds each lane, how busy the card is, and every waiting job with why it waits. `pgpu stats` sums the last 7 days per day (`--days` sets another span): time held, waits and each session's share, the jobs pgpu stopped, and how many jobs ran beside each bench. `pgpu pause` holds new jobs (started ones finish) until `pgpu resume`.

When a bench ends, it prints how many other jobs held a lane beside it and how busy the card was on average.

## Configuration

`~/.config/pgpu/config` (or `$XDG_CONFIG_HOME/pgpu/config`), one `KEY=value` per line, `#` for comments. Every key is optional.

| Key | Default | What it does |
| --- | --- | --- |
| `GPU_SLOTS` | `1` | The lanes: how many jobs run at once, 1 to 4. |
| `GPU_SLOTS_FROM` | none | A file whose `GPU_SLOTS=` line overrides `GPU_SLOTS`, such as a machine-wide env file (`~/` works). |
| `LEND` | `off` | `on` lets a job that holds the card without using it for 3 s lend its lane to the next job; it is frozen until it takes the lane back. |
| `PRIORITY` | `low` | `low` starts each job with `AMD_PRIORITY=-512`, so the desktop's own GPU work goes first; `normal` leaves it alone. |

The queue lives in `$XDG_RUNTIME_DIR/pito-gpu-queue/`, and the logs (`log.tsv`, `pace.tsv`, `killed.tsv`, `benches.tsv`) in `~/.local/state/pito-gpu-queue/`. Programs should read `pgpu status --json` and `pgpu stats --json` rather than these files.

## Part of PITO MD

pgpu is one of the [PITO MD](https://pitomd.com) tools, and the GPU queue the others share on the machine they're built on: [pfx](https://github.com/gmrdad82/pfx) and [Manfred](https://github.com/gmrdad82/manfred) renders and GPU tests, and [pito-codecs](https://github.com/gmrdad82/pito-codecs) GPU benches, run through `pgpu run`. This README's GIFs are recorded with [prec](https://github.com/gmrdad82/prec) and made with [pcod](https://github.com/gmrdad82/pito-codecs), and its chart is drawn by [pito-compare](https://github.com/gmrdad82/pito-compare).

## Contributing

Issues are welcome at [github.com/gmrdad82/pgpu/issues](https://github.com/gmrdad82/pgpu/issues): a bug with the command line and what `pgpu status --json` showed, or an idea. Pull requests aren't taken. Please read the [code of conduct](CODE_OF_CONDUCT.md) first, and report a security issue privately as [SECURITY.md](SECURITY.md) says.

## Build and test

```
cargo build --release
cargo test
```

The tests run the built `pgpu` in sandbox queues under `tmp/` and need cgroup v2 with a delegated cgroup, as a systemd user session gives. CI runs rustfmt, clippy with warnings as errors and the tests on Linux on every push, and a tag builds the release binary and the docs bundle (`bin/docs-bundle`: the pages in `docs/site/` with the command-line reference from `--help`, the JSON spec and the changelog). The README's GIFs are prec shots in `render/terminal.toml`, over sleeping sample jobs, and its chart is pito-compare's from `docs/charts/`.

## Licence

<img src="docs/images/pitomd-wordmark.svg" width="320" alt="PITO MD">

The code is MIT licensed ([LICENSE](LICENSE)), © Catalin Ilinca. The pgpu and PITO MD names and their logos are © Catalin Ilinca, all rights reserved, and are not covered by the MIT licence ([TRADEMARKS.md](TRADEMARKS.md)).
