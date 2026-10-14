---
title: Run a job through the queue
description: Put pgpu run in front of a GPU command, pick its class and session, and read its exit code.
sidebar:
  order: 1
---

Put `pgpu run` and `--` in front of any command that uses the GPU:

```sh
pgpu run --class render --as my-render -- blender -b scene.blend -a
```

`pgpu run` joins the queue, waits for a free lane, runs the command in its own cgroup, and hands the lane to the next job the moment the command ends. Its output is the command's own; pgpu adds a `pgpu: …` line on stderr only when it has something to say (it waits, the job passed its budget, it was stopped).

## Pick a class

`--class` sets the job's priority and its run-time budget. Interactive jobs go first, then tests, clips, renders and benches, then ground-truth renders, then searches:

| Class | Budget | Use it for |
| --- | --- | --- |
| `interactive` | 5 min | previews and anything a person waits on |
| `test` | 10 min | GPU test suites |
| `clip`, `render` | 25 min | clips and renders |
| `bench` | 20 min | benchmarks |
| `truth`, `search` | 1 h | ground-truth renders and long searches |

Without `--class`, pgpu takes the `PGPU_CLASS` environment variable, else `interactive`. An interactive job that runs over 60 s, or never calls `pgpu turn` in its first 5 s, takes turns as a clip, and so does a truth or search job whose waits add up to 10 minutes. See [take turns between passes](../turns/).

## Name the session

`--as` names the session, so its jobs take turns with other sessions' jobs of the same class:

```sh
pgpu run --class test --as ci -- cargo test
```

Without it, pgpu takes the name of the nearest folder holding `.git`. Any character outside `A-Z`, `a-z`, `0-9`, `_` and `.` becomes `_`.

## Set a budget or a memory ceiling

`--budget` sets another run-time budget (`90s`, `15m`, `1h30m`), and `--memory` another memory ceiling (`32G`, `48G`; 24G otherwise). What happens past them is in [budgets and memory ceilings](../budgets-and-memory/).

## What the job sees

The job runs with these in its environment:

| Variable | Value |
| --- | --- |
| `GPU_QUEUE` | `1` |
| `GPU_QUEUE_PID` | the `pgpu run` wrapper's pid |
| `GPU_QUEUE_TURN` | the path `pgpu turn` uses to reach its wrapper |
| `GPU_QUEUE_FROZEN` | a file holding the milliseconds the job has spent frozen or waiting, so it can correct its own timings |

A `pgpu run` inside a job (it sees `GPU_QUEUE`) runs its command directly, so a script that queues itself can be queued again from outside.

## Exit codes

`pgpu run` exits with its job's own code, except:

| Code | Meaning |
| --- | --- |
| 124 | pgpu stopped the job past twice its budget, or stalled |
| 137 | the kernel killed the job at its memory ceiling |
| 128 + n | signal n ended the job or its wrapper |
| 127 | the command couldn't start |
| 2 | the command line didn't parse (clap's error on stderr) |

`--alone` and `--exclusive` are still accepted, so older scripts keep working: they print `pgpu: running alone is gone; this job shares a lane` and the job runs like any other.
