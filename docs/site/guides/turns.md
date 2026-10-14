---
title: Take turns between passes
description: Call pgpu turn between a long job's passes so a waiting job gets in without the long one ending.
sidebar:
  order: 3
---

![A terminal with one GPU lane: a render in passes holds it; an interactive preview job queues and pgpu status shows it waiting its turn; at the render's next pgpu turn the preview runs and prints, then the render takes the lane back.](../../images/turn.gif)

A job that holds a lane keeps it until it ends. A long job that works in passes (frames, tiles, batches, epochs) can call `pgpu turn` between them, so a job that waits gets in without the long one ending.

## Call it between passes

```sh
#!/bin/sh
for pass in 1 2 3 4 5 6; do
  render-pass "$pass"
  pgpu turn
done
```

Run it through the queue with one lane, and queue a preview while it runs:

```sh
pgpu run --class render --as blender -- ./passes.sh &
pgpu run --class interactive --as preview -- ./preview.sh &
pgpu status
```

```
running  blender  ./passes.sh
gpu      44% busy
waiting  interactive  preview        ./preview.sh  (the GPU is busy: waits its turn)
```

At the render's next `pgpu turn`, the preview runs (`preview: frame drawn`), and the render's `pgpu turn` returns once its turn comes back.

## What a turn does

- Outside a `pgpu run` job, `pgpu turn` does nothing and exits 0.
- Inside one, it returns at once when no waiting job comes first. Otherwise it hands the card over and returns when the job's turn comes back. The job is frozen meanwhile, and that time never counts against its budget.
- A job that took the card less than 3 s ago keeps it, unless a job of a higher class waits.
- `pgpu turn` never fails a job: unknown or malformed arguments are ignored.

## Who goes first

Interactive jobs go first, then tests, clips, renders and benches (which take turns as one class), then ground-truth renders, then searches. Within a class, sessions (`--as`) take turns, so two sessions that both queue many jobs share the card instead of one draining the other. An interactive job that runs over 60 s, or never calls `pgpu turn` in its first 5 s, takes turns as a clip, and so does a truth or search job whose waits add up to 10 minutes.

## Report the pacing

`pgpu turn` takes two optional numbers:

```sh
pgpu turn --work-ms 12.4 --longest-ms 18
```

- `--work-ms`: the GPU time the job's own submissions took since its previous turn.
- `--longest-ms`: its longest single submission since then.

They show per session in [`pgpu stats --pace`](../stats/), next to the gaps between turns, which pgpu measures itself.
