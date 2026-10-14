---
title: See how the card was shared
description: Read pgpu stats for each day's use and each session's share, the jobs pgpu stopped, the benches, and each session's pacing.
sidebar:
  order: 7
---

`pgpu stats` sums the last 7 days, one line per day (`--days N` sets another span):

```
stopped  2026-10-14 21:03  sim  interactive  out of memory at 64M after 6.0s (budget 5m00s)  ./hungry.sh
stopped  2026-10-14 21:03  sim  interactive  over twice its budget after 4.0s (budget 2.0s)  ./forever.sh
bench    2026-10-14 21:02  bench  bench  ran 6.0s beside 1 other job, GPU 17% busy  ./bench.sh
day            held   active  busy  holds    wait p50 / p90 / max  sessions
2026-10-14    1m15s    1m00s  123%      8     0.0s / 7.8s / 12.5s  blender 51%, ci 18%, sim 13%, search 8%, bench 7%, preview 0%
```

- `stopped` lines: the jobs pgpu stopped, when, and why (past twice their budget, stalled, or out of memory).
- `bench` lines: each bench, how long it ran, how many other jobs held a lane beside it, and how busy the card was on average meanwhile. A bench prints the same line when it ends.
- The day table:
  - `held`: the lane time jobs held that day, summed over the lanes;
  - `active`: from the day's first hold to its last;
  - `busy`: `held` over `active`, so with two lanes it can pass 100 %;
  - `holds`: how many times a job took a lane;
  - `wait`: the median, 90th percentile and longest time from asking for a lane to holding one;
  - `sessions`: each session's share of `held`, largest first.

## Pacing

`pgpu stats --pace` shows, per session, how often its jobs called `pgpu turn` and what they reported:

```
session          jobs    turns  reported    turn gap p50 / p90 / max     longest p50 / p90 / max
bench               1        0        0%                           -                           -
blender             2        6        0%  5002 ms / 5004 ms / 5004 ms                           -
```

- `turns`: the `pgpu turn` calls; `reported`: the share that carried `--work-ms` or `--longest-ms`.
- `turn gap`: the time between turns, measured by pgpu.
- `longest`: the longest single submission the jobs reported.

A job that turns every few hundred milliseconds lets an interactive job in fast; one that never turns holds its lane to its end. See [take turns between passes](../turns/).

## As JSON

`pgpu stats --json` and `pgpu stats --pace --json` carry the same figures; see [the JSON formats](../../reference/formats/).
