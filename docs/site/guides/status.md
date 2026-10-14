---
title: See what holds the card and why a job waits
description: Read pgpu status, the queue at a glance: the jobs holding lanes, the card's load, and why each waiting job waits.
sidebar:
  order: 4
---

![A terminal: GPU jobs are queued with pgpu run, and pgpu status shows the ones holding the GPU's lanes, how busy the card is, and each waiting job with why it waits; when a running job ends, the next one takes its lane.](../../images/status.gif)

`pgpu status` prints the queue as it is now, in about 1.4 ms:

```
running  blender  ./render.sh
running  ci  ./tests.sh
gpu      14% busy
waiting  bench        bench          ./bench.sh  (all 2 GPU lanes busy: waits its turn)
waiting  search       search         ./search.sh  (all 2 GPU lanes busy: waits its turn)
```

- A `running` line per job that holds a lane: its session (`--as`) and its command.
- `gpu`: how busy the card is right now, from amdgpu.
- A `waiting` line per job in line, in the order pgpu expects to serve them: its class, its session, its command, and why it waits.

With nothing queued:

```
running  nothing
gpu      16% busy
```

## Why a job waits

The reason in brackets is written for a person. The common ones:

| Reason | What it means |
| --- | --- |
| `all 2 GPU lanes busy: waits its turn` | every lane is held; the job takes the next free one that comes to it |
| `the GPU is busy: waits its turn` | the same, with one lane |
| `a GPU lane is free for it: it takes it at its next look` | it's about to start |
| `a GPU lane is free, but jobs ahead of it in line come first: waits its turn` | a job of a higher class, or a session that hasn't been served for longer, goes first |
| `passed over: didn't take a free GPU lane within 3 s; the jobs behind it go first` | it was offered a lane and didn't take it, such as a job stopped with Ctrl+Z |

While the queue is paused, the first line reads `paused` (see [hold the queue](../pause/)).

## Budgets in the status

A job past its run-time budget shows red, with how far over it is:

```
running  sim  ./forever.sh  · over its budget by 0.0s (budget 2s)
gpu      7% busy
```

See [budgets and memory ceilings](../budgets-and-memory/).

## For programs

`pgpu status --json` prints the same as one JSON object, with each job's pid, times and budget, the card's VRAM, and the last hour's use. See [read the queue from a script](../scripts/).
