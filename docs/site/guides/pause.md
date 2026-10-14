---
title: Hold the queue
description: Pause the queue so no new job starts, and resume it.
sidebar:
  order: 8
---

`pgpu pause` holds the queue: jobs that already started run on and finish, and no new job takes a lane until `pgpu resume`.

```sh
pgpu pause
pgpu status
```

```
paused: new jobs hold until resume; started jobs finish
paused
running  nothing
gpu      3% busy
```

Jobs queued meanwhile wait, and `pgpu status --json` gives them `waits_for` `"paused"`. A started job that gave way at a turn may come back and finish during a pause.

```sh
pgpu resume
```

```
resumed
```

The waiting jobs then take the lanes in their usual order. The pause lives in the queue's folder, so it holds across terminals and ends at logout with the queue itself.
