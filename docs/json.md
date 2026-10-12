# pgpu's JSON

pgpu's outs for other programs are three JSON commands: `pgpu status --json`, `pgpu stats --json` and `pgpu stats --pace --json` (`pgpu stats --play --json` until v0.29.0). This file is their spec, and pgpu keeps it current with every change. How a reader draws a field is the reader's own.

## Rules

- Readers use these commands, never pgpu's state files.
- Every change is additive: a field or a value may be added, and none is renamed, retyped or removed.
- A reader may call as often or as rarely as it likes. Nothing in pgpu depends on a caller's cadence: the orphan rescue runs at every pgpu call. Since v0.24.0 a `status --json` a second apart costs about 1.3 ms of pgpu's own CPU, and about 2 ms with process start.
- pgpu is one queue per machine, and its JSON carries no `host`; a reader that gathers several machines adds it.
- Times are Unix ms. Each object starts with `"v":1`.

## `pgpu status --json`

One object. An example (v0.29.0, two lanes, labels shortened):

```json
{"v":1,"paused":false,"gpu_busy":96,"play":null,"away":null,"away_reason":null,"desk":null,
 "vram":{"used_mib":9300,"total_mib":16304},
 "jobs":[
  {"pid":1440,"state":"running","position":0,"class":"clip","as":"pfx","label":"pfx render …","realtime":false,"arrived":1790000000100,"since":1790000000123,"waits_for":null,"why":null,
   "alone":false,"budget_ms":1500000,"ran_ms":61000,"over_ms":null,"frozen_ms":0,"idle_ms":0,
   "pace":{"turn_gap_ms":61.0,"work_ms":12.4,"longest_ms":18.0},"lane":{"group":"gpu","worker":0,"path":"/work/job","started":1790000000050}},
  {"pid":1450,"state":"running","position":1,"class":"interactive","as":"manfred","label":"manfred render …","realtime":false,"arrived":1790000000110,"since":1790000000140,"waits_for":null,"why":null,
   "alone":false,"budget_ms":300000,"ran_ms":340000,"over_ms":40000,"frozen_ms":1200,"idle_ms":4100,"pace":null,"lane":{"group":"gpu","worker":1,"path":"/work/m","started":1790000000060}},
  {"pid":1511,"state":"waiting","position":2,"class":"search","as":"paper","label":"…","realtime":false,"arrived":1790000000300,"waits_for":"turn","why":"all 2 GPU lanes busy: waits its turn",
   "alone":false,"budget_ms":null,"ran_ms":null,"over_ms":null,"frozen_ms":null,"idle_ms":null,"pace":null,"lane":null}],
 "hour":{"held_pct":62,"jobs":9,"holds":140,"gave_way":31,"wait_ms":{"p50":1200,"p90":13600,"max":95000}},
 "outside":[]}
```

Top level:

- `v`: always 1.
- `paused`: true while `pgpu pause` is in force.
- `gpu_busy`: the card's busy percent, or null.
- `play`, `away`, `away_reason` and `desk`: always null since v0.29.0: pgpu no longer detects games, presence or away, and keeps no desk share. The keys stay so readers that look for them don't break. Before, `play` was an object (`game`, `share`, `auto`, `hold`, `in_front`, `game_gpu`, `slice_ms`, `vrr`) while a game ran, `away` true, false or null, `away_reason` `idle`, `locked` or `watching`, and `desk` `{"share":70}`.
- `vram`: `{"used_mib":…,"total_mib":…}` for the card, or null.
- `jobs`: the jobs in the order pgpu expects to serve them: the holder first, then the rest as `acquire()` ranks them (class, a `truth` or `search` job whose waits add up to 10 minutes ranking as `clip`; then when the session was last served, when the job was, then arrival). It is a prediction: a higher class that arrives later goes ahead, and the holder's session keeps its old served stamp while it holds, so a second ticket of that session can be listed ahead of a job of another session that will go first. Each job:
  - `pid`: the `pgpu run` wrapper's pid. A caller that records its job keeps this pid.
  - `state`: `running` holds the GPU; `waiting` hasn't held it yet, or is held back; `yielded` gave way at `pgpu turn` and is blocked there until its turn comes back; `lent` held the GPU without using it, lent it to the next job, and keeps running on the CPU (its `waits_for` is null, and it is never the holder); since v0.30.4 a job that runs alone (`alone` true) is `lent` while it lends its lanes, and takes them back by freezing the jobs it lent them to, which show `waiting` with `waits_for` `"alone"` meanwhile.
  - `position`: 0 for the first holder; with two GPU lanes the jobs holding them come first, at 0 and 1.
  - `class`: `interactive`, `test`, `clip`, `render`, `bench`, `truth` or `search` (since v0.29.0; `test`, `render` and `bench` take turns as `clip`): the job's own class, also once it takes turns as a `clip` for having waited 10 minutes, run 60 s as `interactive`, or never called `turn` in its first 5 s.
  - `alone` (v0.29.0): true for a re-run with `--alone` (v0.30.9; `--exclusive` before it, and, before v0.30.8, every `bench` job), which waits for the GPU to clear and lets no other job run beside it.
  - `budget_ms`, `ran_ms`, `over_ms`, `frozen_ms` (v0.29.0): the job's budget (`--budget`, else its class's: interactive 5 min, test 10 min, clip and render 25 min, bench 20 min, truth and search 1 h), the time it has run (holding or lent, not frozen and not waiting), how far past its budget it is, null until it is, and `frozen_ms`, the time since it started that it didn't run (frozen, or waiting for the GPU after a turn or a lend), which skews any wall-clock number the job takes; since v0.30.2 it is the same number the file `GPU_QUEUE_FROZEN` names holds, counted from the freezes and the turns' waits. All four are null for a job that hasn't started. At twice its budget, or after 5 minutes of run time with no GPU work, no `turn` and under 5 % of a CPU, pgpu stops it (`pgpu stats --json`'s `killed`).
  - `idle_ms` (v0.30.4): how long the job's own `gfx` and `compute` use has stayed under 5 % at every look, 0 while it uses the GPU (or has video work or `/dev/kfd` open), null before it starts or from an older wrapper. A job that runs alone lends its lanes once it reaches 10 s while a job could start; `status` notes `idling the card` on a running one from 3 s.
  - `why` (v0.29.0): why a job that isn't running waits, in words for a person (`"all 2 GPU lanes busy: waits its turn"`, `"pfx runs alone (bench)"`), or null; a reader shows it as it is.
  - `as`: the session name. Every character outside ASCII `[A-Za-z0-9_.]` becomes one `_`; with no `--as` it is the nearest ancestor folder holding `.git`, or `unknown`; an empty name, `.` or `..` is `unknown`; a name is cut to 255 bytes.
  - `label`: the full command line.
  - `realtime`: always `false`.
  - `arrived`: when the job joined the queue. The holder also has `since`, when it last took the GPU.
  - `waits_for`: why a job that isn't running waits, or null:
    - `"paused"`: the queue is paused and the job hasn't started (a started job that gave way may come back and finish during a pause);
    - `"stopped"`: its wrapper is stopped by a signal (Ctrl+Z, a debugger, a paused pueue task) and is passed over until it continues;
    - `"alone"` (v0.29.0): a job that runs alone holds the GPU or is ahead of it, or the job itself runs alone and waits for the others to give way;
    - `"turn"` (v0.29.0): every lane is busy and the job waits its turn.
    Before v0.29.0 a job waiting its turn carried null, and `"vram"` (the video-memory guard), `"desk"` (the desk guard) and `"play"` (the play rule) were sent; none is sent since. `waits_for` never sends `"game"`.
  - `pace`: null for a job that never turned, otherwise `{"turn_gap_ms","work_ms","longest_ms"}` over its last 10 s of turns: the longest gap between turns (for the running job, the time since its last turn counts too), the mean reported work per turn, and the longest reported submission, each null when nothing was reported. Reports are clamped to the time the job ran, so a freeze never reads as work or as a gap. The limits every GPU caller agreed to (HANDOVER §4, the pacing contract): `pgpu turn` never more than 500 ms apart, and no single submission of 50 ms or more.
  - `lane`: `{"group","worker","path","started"}`, from the wrapper's `PUEUE_GROUP` (verbatim) and `PUEUE_WORKER_ID` (a number, null when unreadable), or null outside pueue. `path` is the working folder of the task's shell (pueue's `sh -c`, the wrapper's topmost ancestor below `pueued`; the same folder as the task's `path` in pueue's JSON, with the kernel's ` (deleted)` mark dropped) and `started` that shell's start (within about a second of pueue's `Running.start`), so a reader can match a job to its pueue task by group, then `path`, then `started` within 2 s. Both are null when no `pueued` is found above the wrapper within 64 steps, or the shell can't be read.
- `hour`: pgpu's use of the last 60 minutes, or null when no hold touches them: `held_pct` (the share of the hour some job held the GPU, the hold in progress included), `jobs` (the distinct wrappers that held it), `holds` (the holds that ended in the hour), `gave_way` (those that ended at a turn, a desk hold, a play hold, a lend or a bench's reclaim, rather than with the job), and `wait_ms` (`p50`, `p90`, `max` of the time from asking to holding). pgpu caches it for 10 s.
- `outside`: processes in a build lane that use the GPU without pgpu, each `{"pid","comm","gpu_pct","vram_mib","lane","since"}`. A process counts when it is in `build.slice` (where pueued and its tasks run), has no `GPU_QUEUE` in its environment, and its `gfx` plus `compute` use, added together, is at least 5 %, each DRM client counted once. `gpu_pct` is 0–100, `vram_mib` its `drm-memory-vram`, `lane` in the same shape as a job's (filled from the process), and `since` when it first crossed 5 %. The use is measured between this call and a cache 0.5–10 s old; with no such cache it is `[]`, and the next call has numbers. A process that starts using the GPU shows within about 5 s.

## `pgpu stats --json`

`{"v":1,"days":[…],"killed":[…],"benches":[…]}`, one object per local day with a hold in the last `--days` days (7 by default): `day` (the local date, `2026-10-03`), `held_ms`, `active_ms` (from the day's first hold to its last), `busy` (`held_ms` over `active_ms`, in percent), `holds`, `wait_ms` (`p50`, `p90`, `max`), and `sessions`, each `{"as","held_ms","share"}` with `share` its percent of the day's `held_ms`, largest first. The figures match the text `stats`.

`benches` (v0.30.9) lists the benches and `--alone` re-runs that ran in those days, oldest first, each `{"at","as","class","pid","ran_ms","shared","gpu_busy","alone","label"}`: `at` when it ended, `pid` its wrapper's, `ran_ms` its run time, `shared` how many other jobs held a lane beside it while it held one, `gpu_busy` the card's average busy percent over its looks (null if unread), `alone` the `--alone` reason or null.

`killed` (v0.29.0) lists the jobs pgpu stopped in those days, oldest first, each `{"at","as","class","pid","budget_ms","ran_ms","memory_bytes","why","label"}`: `at` when it was stopped, `pid` its wrapper's, `memory_bytes` (v0.30.0) its memory ceiling (`--memory`, else its class's, 24 GiB), null for a row from before v0.30.0 or a job that had none, `why` `"budget"` (it ran twice its budget), `"stall"` (5 minutes of run time with no GPU work, no `turn` and under 5 % of a CPU) or `"memory"` (v0.30.0: the kernel killed it at its memory ceiling, or for memory), and `label` its command line. Its `pgpu run` printed one `pgpu: stopped …` line and exited 124, or 137 for `"memory"`.

## `pgpu stats --pace --json`

`{"v":1,"days":N,"sessions":[{"as","jobs","turns","reported_pct","turn_gap_ms","longest_ms"}]}`, one object per session over the jobs that ended in the last N days: `jobs`, `turns`, `reported_pct` (the share of turns that carried a report), and `turn_gap_ms` and `longest_ms`, each `{"p50","p90","max"}` or null.

## `pgpu stats --play --json` (until v0.29.0)

Removed in v0.29.0 with the play rule: `--play` is now clap's error. It printed `{"v":1,"sessions":[…]}`, one object per play session.

## What came when

Each step added; none renamed or removed a field.

- v0.3.0: `desk`. The share then came from `GPU_QUEUE_DESK` or a `desk` file, 80 by default; since v0.16.0 it is the `share` knob, 70 by default.
- v0.6.0: top-level `vram`; `play.in_front` and `play.game_gpu`; `jobs[].waits_for` with `"paused"` and `"vram"`.
- v0.7.0: `play.vrr`; `play.game` as the Steam game's real name; `play.in_front` reported whatever the job.
- v0.8.0 (agreed 2026-10-01): `hour`. A pgpu without the key is older.
- v0.9.0: `play.hold`.
- v0.10.0 (2026-10-02): `share=auto`, `--realtime` and the slice knob went, and the JSON stayed: `play.auto` is always false, `play.slice_ms` always 250, `jobs[].realtime` always false, and `waits_for` never `"game"`.
- v0.11.0: no field. `status` drops a holder whose wrapper is gone; jobs of one `--as` take turns; `"paused"` marks only jobs that haven't started.
- v0.14.0 (2026-10-02): `waits_for` `"desk"`. The hour's `gave_way` counts desk hand-overs.
- v0.16.0 and v0.16.1 (agreed 2026-10-03): `away`, `jobs[].pace`, `jobs[].lane` (`path` and `started` in v0.16.1, in `outside[].lane` too), `outside`, and `stats --pace --json`.
- v0.17.0 (agreed 2026-10-03): `state` `"lent"`. The hour's `gave_way` counts lends.
- v0.18.0 (2026-10-04): `waits_for` `"stopped"`; `--as` of `""`, `.` or `..` becomes `unknown`, and a name is cut to 255 bytes.
- v0.19.0 (2026-10-04): `play.share` null while the game isn't in force; `play.game_gpu` null when the last play tick is over 5 s old.
- v0.21.0 (2026-10-05): the play rule. `waits_for` `"play"`; `play.share` is the play share (30 by default, 0 under `hold`, the desk share under `play=0`); `play.hold` also for `share=hold` with no `play` file. Before it, play mode paused and resumed the holder several times a second, and a job held back for a game showed only as `waiting` at the head.
- v0.22.0 (2026-10-05): `away_reason`, with the `watch` rule.
- v0.24.0 (2026-10-05): no field; a call a second apart costs much less.
- v0.27.0 (2026-10-06): no field; `play.game` may name a process announced with `pgpu play on`.
- v0.29.0 (2026-10-12): as many jobs run at once as the GPU lanes (`GPU_SLOTS`), each holder listed `running`; `class` values `test`, `render` and `bench`; `jobs[].alone`, `budget_ms`, `ran_ms`, `over_ms`, `frozen_ms` and `why`; `waits_for` `"alone"` and `"turn"`; `stats --json`'s `killed`. The desk share, desk guard, play rule and presence went: `play`, `away`, `away_reason` and `desk` are always null, `waits_for` no longer sends `"vram"`, `"desk"` or `"play"`, and `stats --play` is gone.
- v0.30.0 (2026-10-12): `stats --json`'s `killed[].memory_bytes` and `why` `"memory"`.
- v0.31.2 (2026-10-12): no field. The lanes are `GPU_SLOTS`, never more, so at most that many jobs are `running`; a waiter's `why` with every lane taken reads `all <n> GPU lanes busy: waits its turn` again.
- v0.31.1 (2026-10-12): no field. A waiter that didn't take a free lane within 3 s is passed over by the jobs behind it until it holds a lane, and reads `waits_for` `"turn"` with `why` `passed over: didn't take a free GPU lane within 3 s; the jobs behind it go first`.
- v0.30.10 (2026-10-12): no field. A waiter's `why` with every lane taken reads `all <n> lanes busy, card <busy>%: waits its turn (a lane opens under 70%)` while fewer than 4 lanes are open, and the lane count can grow past `GPU_SLOTS` to 4, so more than two jobs can be `running`.
- v0.30.9 (2026-10-12): `stats --json`'s `benches`; `why` `re-runs alone: waits for an empty queue or the night (01:00 to 07:00)` (with `waits_for` `"alone"`) on a waiting `--alone` re-run by day; the 0.30.7 backfill `why` is gone.
- v0.30.8 (2026-10-12): no field; a `bench` job is no longer `alone` unless run with `--exclusive`, so its `waits_for` is `"turn"` like any other class.
- v0.30.7 (2026-10-12): a bench waiting for the card to clear reads `why` `backfilling beside a waiting bench: <s> s left, then the card drains for it` (with `waits_for` `"alone"`) for its first 60 s, while jobs behind it may start in free lanes.
- v0.30.5 (2026-10-12): a job that isn't `running` or `lent` always has `waits_for` and `why`: a waiter with a lane free for it reads `"turn"` and `a GPU lane is free for it: it takes it at its next look`, and `turn` with a free lane reads `a GPU lane is free, but jobs ahead of it in line come first: waits its turn` instead of `all 2 GPU lanes busy`.
- v0.30.4 (2026-10-12): `jobs[].idle_ms`, and a job that runs alone can be `lent`. The hour's `gave_way` counts `reclaim` ends (a borrower giving a bench its lanes back).
