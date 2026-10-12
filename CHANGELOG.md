# Changelog

## v0.31.1 — 2026-10-12

### Fixed

- A waiting job that doesn't take a free GPU lane within 3 s, such as one queued under an older pgpu, is passed over by the jobs behind it, so it no longer stalls the queue.

## v0.31.0 — 2026-10-12

### Changed

- pgpu reads its settings from ~/.config/pgpu/config: GPU_SLOTS, GPU_SLOTS_FROM (a file whose GPU_SLOTS line overrides it), LEND and PRIORITY.
- Any AMD GPU on amdgpu with a busy reading drives the lanes, the one with the most VRAM when there are several.
- A waiting job's reason and a bench's end line are shorter: "all 2 lanes busy, card 9%: waits its turn (a lane opens under 70%)".

### Removed

- Crash reports: pgpu sends nothing anywhere.

## v0.30.11 — 2026-10-12

### Fixed

- No GPU lane opens past GPU_SLOTS while a job queued under an older pgpu still waits, since its wrapper counts only GPU_SLOTS and would wait behind the new lanes.

## v0.30.10 — 2026-10-12

### Changed

- The GPU lanes follow the card: GPU_SLOTS is the floor, and one more lane opens (one per 10 s, up to 4) while jobs wait, the card averages under 70% busy over 10 s and VRAM is under 80%; over 90% or with VRAM over 80% no new lane opens, and a held lane is never taken back. Ordinary jobs no longer lend their lane by default.

## v0.30.9 — 2026-10-12

### Added

- Every finished bench, and every re-run alone, says how many other jobs held a lane beside it and how busy the GPU was on average, on stderr and in pgpu stats (stats --json's benches).

### Changed

- Running alone is only for re-running a number that looks skewed: --alone "<why>" replaces --exclusive, which is refused bare. By day the re-run waits for an empty queue; at night nothing new starts behind it while running jobs finish, so it never jumps the line or makes a job yield. The 60 s backfill beside a waiting bench is gone.

## v0.30.8 — 2026-10-12

### Changed

- A bench takes a GPU lane like any other class and runs beside the rest: it no longer runs alone, drains the card or makes running jobs yield to it. Running alone stays the opt-in --exclusive.

## v0.30.7 — 2026-10-12

### Changed

- Jobs backfill free lanes beside a waiting bench (or --exclusive job) for 60 s after it arrives and keep them at their turns; then nothing new starts, holders hand over at their next turn, and the bench runs alone once the card clears. pgpu status says "backfilling beside a waiting bench: 42 s left" on the bench meanwhile.

## v0.30.6 — 2026-10-12

### Fixed

- On the live queue pgpu reads its config and state from the user's own home whatever XDG_CONFIG_HOME and XDG_STATE_HOME say, so a job run inside a prec tape's home (HOME and both XDG folders pointed there) sees both GPU lanes and logs to the real log.tsv; 0.30.5 covered HOME alone. A queue of its own (its own XDG_RUNTIME_DIR) still uses the XDG folders.

## v0.30.5 — 2026-10-12

### Fixed

- Two lanes run two jobs again when a job is started with HOME pointed elsewhere (a prec tape): pgpu reads its config and state from the user's home in the password database, not $HOME, so such a job no longer sees one lane, waits while the other sits free, or logs its holds outside log.tsv.
- Every waiting job in pgpu status has a waits_for and a reason, and "all GPU lanes busy" is said only when they are.

## v0.30.4 — 2026-10-12

### Changed

- A bench job, or one run with --exclusive, lends its lanes once its own GPU use has stayed under 5% for 10 s while a job could start, and takes them back the moment it uses the GPU again by freezing the jobs it lent them to; it is never frozen itself. A borrower's hold ends as reclaim in log.tsv.

### Added

- pgpu status notes a bench idling the card, lists a lent bench as lent, and status --json gives every job's idle_ms.
- A job that runs alone prints at its end how long it held the GPU alone, how busy the GPU was in that time, and how often it lent its lanes.

## v0.30.3 — 2026-10-12

### Fixed

- A job never outlives or leaves its cell: an interrupted pgpu run signals every process of the job and kills what is left after 2 s, a job's leftovers are killed when its command ends, and a wrapper killed with SIGKILL has its job killed by the next pgpu call; before, they were moved out of the cell and ran on past the memory ceiling.

## v0.30.2 — 2026-10-12

### Fixed

- A bench job, or one run with --exclusive, keeps every lane until it ends: it never lends the GPU during its CPU steps and never hands it over at a turn, so it is never frozen waiting for the card.
- The file GPU_QUEUE_FROZEN names counts the time a job waits in pgpu turn for the GPU, not only freezes, and holds the same number as the end-of-run line and status --json's frozen_ms.

## v0.30.1 — 2026-10-12

### Fixed

- The stall check counts the job's whole process tree, also processes that left its cell (a systemd-run --scope inside the job), and a stop kills them too; such a job read as idle and was stopped after 5 minutes while it worked.

## v0.30.0 — 2026-10-12

### Added

- A memory ceiling for every job: 24G for every class, --memory to set another; the kernel slows the job at 15/16 of it and kills the whole job past it, with no swap.
- A job killed at its ceiling prints why, exits 137, and is recorded as memory in log.tsv and stats --json's killed, which gains memory_bytes.

### Changed

- A job's cgroup sits beside its wrapper's, in the same slice, so it gets the memory controller.

## v0.29.0 — 2026-10-12

### Added

- GPU lanes: as many jobs hold the GPU at once as there are GPU lanes (GPU_SLOTS).
- A bench job, or one run with --exclusive, runs alone and keeps the jobs behind it out of a free lane.
- Budgets: every run has one (--budget, else its class's); past it a job shows red, and at twice it or after 5 minutes with no GPU work, turn or CPU it is stopped and exits 124.
- Frozen time is reported: a job frozen or waiting for the GPU a second or more says so on stderr when it ends, and status --json has frozen_ms.
- Classes test, render and bench; status --json's why, alone, budget_ms, ran_ms, over_ms and frozen_ms; waits_for alone and turn; stats --json's killed.

### Changed

- An interactive job that has run 60 s, or never calls turn in its first 5 s, takes turns as a clip.
- A job waiting its turn says why in status instead of nothing.
- pgpu uses the GPU all the time: the desk share, the desk guard, presence and away detection, the play rule and game freeze, the video-memory guard, render-CPU confinement, pgpu play and stats --play are removed with their knobs; status --json keeps play, away, away_reason and desk as null.
