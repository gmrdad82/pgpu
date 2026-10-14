---
title: Install
description: Install pgpu on Linux with an AMD GPU, set its lanes, and run a first job.
sidebar:
  order: 2
---

pgpu runs on Linux with an AMD GPU on the amdgpu driver, and nowhere else: no Windows, no macOS, no other GPU vendor.

## What it needs

- Linux x86_64 with cgroup v2 and a systemd user session, so each job gets its own cgroup for its memory ceiling and for freezing. Where pgpu can't make one, it pauses a job with SIGSTOP instead, runs it without a ceiling, and says so.
- An AMD GPU on the amdgpu driver. pgpu reads the card's busy percent and VRAM from amdgpu's sysfs files and each job's GPU use from DRM fdinfo. With several amdgpu cards, it watches the one with the most VRAM.

## Linux x86_64

Each [release](https://github.com/gmrdad82/pgpu/releases/latest) carries `pgpu-vX.Y.Z-linux-x86_64.tar.gz` (the binary, `LICENSE` and `README.md`) and a `SHA256SUMS` file. These two lines install the latest into `~/.local/bin`, which needs to be on your `PATH`:

```sh
v=$(basename "$(curl -fsSLo /dev/null -w '%{url_effective}' https://github.com/gmrdad82/pgpu/releases/latest)")
mkdir -p ~/.local/bin && curl -fsSL "https://github.com/gmrdad82/pgpu/releases/download/$v/pgpu-$v-linux-x86_64.tar.gz" | tar -xz -C ~/.local/bin pgpu
```

To check the download first, fetch the archive and `SHA256SUMS` into one folder and run `sha256sum -c --ignore-missing SHA256SUMS` there.

## From source

With Rust 1.97 or newer:

```sh
cargo install --locked --git https://github.com/gmrdad82/pgpu --tag vX.Y.Z pgpu
```

Replace `vX.Y.Z` with a [tag](https://github.com/gmrdad82/pgpu/tags).

## Quick start

One lane is the default: one job holds the card at a time. For two, write the config file:

```sh
mkdir -p ~/.config/pgpu && echo GPU_SLOTS=2 > ~/.config/pgpu/config
```

Put `pgpu run --` in front of GPU commands, from as many terminals or scripts as you like, and look at the queue:

```sh
pgpu run --class render --as blender -- ./render.sh &
pgpu run --class test --as ci -- ./tests.sh &
pgpu run --class bench --as bench -- ./bench.sh &
pgpu run --class search --as search -- ./search.sh &
pgpu status
```

```
running  blender  ./render.sh
running  ci  ./tests.sh
gpu      14% busy
waiting  bench        bench          ./bench.sh  (all 2 GPU lanes busy: waits its turn)
waiting  search       search         ./search.sh  (all 2 GPU lanes busy: waits its turn)
```

Two jobs hold the two lanes and the other two wait. When one ends, the next takes its lane at once. Each waiting `pgpu run` also prints `pgpu: waiting (…)` on stderr when it starts to wait.

Next: [run a job](../guides/run-a-job/), and [set the lanes and the other knobs](../guides/configure/).

## Remove it

Delete the binary (`rm ~/.local/bin/pgpu`), the config (`~/.config/pgpu/`) and the logs (`~/.local/state/pito-gpu-queue/`). The queue's own folder, `$XDG_RUNTIME_DIR/pito-gpu-queue/`, goes away at logout.
