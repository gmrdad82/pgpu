---
title: Set the lanes and the other knobs
description: How many jobs hold the card at once, lending an idle lane, and the GPU priority jobs start with.
sidebar:
  order: 2
---

pgpu reads one file, `~/.config/pgpu/config` (or `$XDG_CONFIG_HOME/pgpu/config`): one `KEY=value` per line, `#` for comments. Every key is optional. There is no daemon to restart: each `pgpu` call reads the file itself.

```sh
GPU_SLOTS=2
PRIORITY=low
```

| Key | Default | What it does |
| --- | --- | --- |
| `GPU_SLOTS` | `1` | The lanes: how many jobs run at once, 1 to 4. |
| `GPU_SLOTS_FROM` | none | A file whose `GPU_SLOTS=` line overrides `GPU_SLOTS`, such as a machine-wide env file (`~/` works). |
| `LEND` | `off` | `on` lets a job that holds the card without using it for 3 s lend its lane to the next job; it is frozen until it takes the lane back. |
| `PRIORITY` | `low` | `low` starts each job with `AMD_PRIORITY=-512` (unless the job already has one), so the desktop's own GPU work goes first; `normal` leaves it alone. |

Values may be quoted, and when a key appears twice the last line wins.

## Lanes

`GPU_SLOTS` jobs run at once, never more, however idle the card. A free lane goes to the first job in line, by class and then by session (see [take turns between passes](../turns/)). A waiter that doesn't take its free lane within 3 s, such as one that was stopped with Ctrl+Z, is passed over, so whatever waits fills the lanes. A running job never loses its lane, and nothing runs alone.

How many lanes suit a card depends on its VRAM and the jobs: each job holds its own memory on the card, and pgpu doesn't count VRAM against the lanes. Two is a good start on a 16 GiB card; [pgpu status](../status/) shows the card's busy percent and VRAM while jobs run.

## One setting for several tools

`GPU_SLOTS_FROM` lets another file own the lane count, so tools that read the same machine file agree:

```sh
GPU_SLOTS_FROM=~/.config/machine.env
```

pgpu reads the `GPU_SLOTS=` line of that file, and falls back to its own `GPU_SLOTS` when the file or the line is missing.

## Where the queue lives

The queue is a folder of tickets in `$XDG_RUNTIME_DIR/pito-gpu-queue/`, and the logs (`log.tsv`, `pace.tsv`, `killed.tsv`, `benches.tsv`) live in `~/.local/state/pito-gpu-queue/`. A program should read [`pgpu status --json` and `pgpu stats --json`](../scripts/) rather than these files.

A shell with its own `XDG_RUNTIME_DIR` gets a queue of its own, with its config and logs under its own `XDG_CONFIG_HOME` and `XDG_STATE_HOME`: handy for trying pgpu out without touching the real queue. On the real queue (the runtime folder under `/run/user/`), pgpu always reads the config and writes the logs in the user's own home, whatever `HOME` or the XDG variables a job's script set.
