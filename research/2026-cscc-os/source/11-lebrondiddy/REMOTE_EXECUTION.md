# Remote Execution

> ⚠️ **Superseded 2026-06-17 — WSL retired.** The old WSL box
> (`lenovo@100.92.143.96`) is **retired and unreachable**. The current remote
> build/regression target is the school server: `ssh yxzhang@10.102.35.44`
> (Ubuntu x86_64, 80c / 251G, official image `zhouzhouyi/os-contest:20260510`).
> Live workflow + one-time bootstrap: see `PLAN.md` §0 / §0.5. Sync is Mutagen
> `oskernel-school-sync` (Mac ↔ `/home/yxzhang/oskernel2025-x`). Everything below
> is the historical WSL/Hermes setup record — kept for provenance, do not follow.

## Historical: WSL setup (2026-06-08, retired)

Primary execution target (historical):

```text
ssh -p 2222 lenovo@100.92.143.96
```

The user's intended setup is:

- Mac workspace is synced to the remote WSL/server through Mutagen.
- The Codex executor that builds and tests kernel code should run on the remote.
- Official contest Docker image should be used remotely:

```text
zhouzhouyi/os-contest:20260510
```

## Why Remote Is Ground Truth

The local Mac is Darwin/arm64 and should not be treated as the contest build
truth. The contest environment pins Docker/QEMU/toolchain versions, including
QEMU 9.2.1 and cross toolchains for RISC-V and LoongArch.

## Required Verification

Before kernel implementation tasks begin, record:

- Remote workspace path.
- `uname -a` on remote.
- Docker availability.
- Whether `zhouzhouyi/os-contest:20260510` is present or can be pulled.
- Exact docker command used for the harness.

## Verification: 2026-06-08

Command target:

```text
ssh -p 2222 lenovo@100.92.143.96
```

Observed remote state:

- Hostname: `LAPTOP-AM2436B4`
- Remote home: `/home/lenovo`
- Kernel: WSL2 Linux `6.6.87.2-microsoft-standard-WSL2`
- Architecture: `x86_64`
- `git`: `/usr/bin/git`
- `make`: `/usr/bin/make`
- `docker`: not found in remote PATH
- `podman`: not found in remote PATH
- `mutagen`: not found in remote PATH
- `qemu-system-riscv64`: not found in remote PATH
- Official image `zhouzhouyi/os-contest:20260510`: not present remotely

Observed local Mac state:

- `mutagen`: `/opt/homebrew/bin/mutagen`
- Existing Mutagen sessions sync `/Users/yuxuanzhang/Desktop/2025-2026-2/fixture`
  to `/home/lenovo/fixture`; this kernel workspace is not currently synced by
  those sessions.
- Local Docker exists at `/usr/local/bin/docker`, but local Mac/Darwin should
  not be treated as the contest ground truth.

Conclusion:

Remote WSL is reachable and is the right architecture, but it currently lacks
Docker/QEMU in PATH. Before kernel implementation starts, either install/enable
Docker on WSL or use another remote execution path that can run the official
contest image. A dedicated Mutagen sync session or GitLab clone/pull workflow is
also needed for this project directory.

Follow-up on 2026-06-08:

- Windows Docker CLI exists at
  `/mnt/c/Program Files/Docker/Docker/resources/bin/docker.exe`.
- Docker daemon is not running or WSL integration is unavailable. The CLI reports:
  `failed to connect to the docker API at npipe:////./pipe/docker_engine`.
- Attempting to start Docker Desktop from SSH did not make the daemon available
  within 60 seconds.

Action needed:

- Start Docker Desktop on the Windows host and enable WSL integration for this
  distro, or install Docker Engine inside WSL.
- Then verify:

```sh
ssh -p 2222 lenovo@100.92.143.96
"/mnt/c/Program Files/Docker/Docker/resources/bin/docker.exe" info
"/mnt/c/Program Files/Docker/Docker/resources/bin/docker.exe" pull zhouzhouyi/os-contest:20260510
```

## Mutagen Sync: 2026-06-08

Created a dedicated Mutagen session for this project:

```text
Name: oskernel-sync
Identifier: sync_mDAfrwdJi2WefGQTohgHjlsCeQfAjXyx79pgiNSdDo5
Alpha: /Users/yuxuanzhang/Desktop/2025-2026-2/操作系统/内核实现赛
Beta:  wsl-fixture:/home/lenovo/oskernel2025-x
Mode:  default Mutagen sync mode
VCS:   included via --no-ignore-vcs
Ignore:
  .DS_Store
  kernel-rv
  kernel-la
  disk.img
  disk-la.img
```

Verification:

- Session status: `Watching for changes`
- Remote path: `/home/lenovo/oskernel2025-x`
- Remote git branch: `main`
- Remote latest commit: `8f0cd24 TASK-002 record gitlab push blocker`

Operational rule:

After each local change, commit locally first. Mutagen will sync the commit and
working tree to WSL. Push to GitLab once project permissions are fixed.

## Draft Docker Command

This command is a starting point and should be verified on remote:

```sh
docker run --rm -it \
  -v "$PWD:/workspace" \
  -w /workspace \
  zhouzhouyi/os-contest:20260510 \
  bash
```

## Hermes Remote Codex Automation: 2026-06-08

Remote Codex is available at `/usr/local/bin/codex` (`codex-cli 0.136.0`).
Hermes dispatch now uses:

```text
scripts/hermes-create-task.sh TASK-XXX
scripts/hermes-dispatch.sh .hermes/outbox/TASK-XXX-YYYYMMDD-HHMMSS.md
```

The dispatch script transfers the task packet to
`/home/lenovo/oskernel2025-x/.hermes/inbox/`, runs remote `codex exec` in
`/home/lenovo/oskernel2025-x`, and stores logs under `.hermes/runs/` on both
machines. See `docs/HERMES_AUTOMATION.md`.
