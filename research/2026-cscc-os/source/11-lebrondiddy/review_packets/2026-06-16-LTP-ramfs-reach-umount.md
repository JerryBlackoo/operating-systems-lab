# 2026-06-16 LTP ramfs reach / umount cleanup

## Goal

Fix the LTP reach wall where repeated cases retained ramfs files / mounts until the
1G test VM ran out of frames around 700-730 cases.

Do not count TBROK garbage as score. LTP score evidence below uses only
`FAIL LTP CASE <name> : 0` / `initproc ltp: ... returned 0`.

## Changes

- `kernel/filesystem/fs/src/dentry.rs`
  - `mount_fs()` is now idempotent for an existing path. Before pushing a new
    `(path, fs)` entry, it removes the old entry for the same path.
  - Root cause: `refresh_runtime_mounts()` remounts `/tmp`, `/dev/shm`, `/var`,
    `/proc`, etc. Repeated mounts kept old ramfs roots reachable through
    `MOUNTED_FS`, so clearing directories did not release all frames.
- `kernel/kernel/src/tasks/initproc.rs`
  - After each LTP case, clear `/tmp`, `/dev/shm`, `/var/tmp`, `/var/lock`,
    `/run`, `<work_dir>/tmp`, and `<work_dir>/mntpoint`.
  - While recursively clearing a directory, try `umount(child_path)` before
    removing the child. This also releases test-created scratch mounts such as
    `/tmp/LTP.../mntpoint`.
  - `prepare_ltp_rootfs()` creates `/var/tmp`, `/var/lock`, and `/run`.
- `kernel/filesystem/fs/src/lib.rs`
  - `/dev/vda2` scratch mount now uses a matching scratch umount callback,
    rather than sharing the ext4-only callback by name.
- `kernel/filesystem/ramfs/src/lib.rs`
  - Existing session fix kept: `writeat()` / `truncate()` return `ENOSPC` and
    roll back newly allocated pages instead of panicking with
    `can't alloc frame in ram fs`.

## Build / ABI gate

Log: `.codex-logs/clean-build-ramfs-reach-umount-20260616.log`

Command:

```sh
rm -rf kernel/target kernel-rv kernel-la kernel/mount.img
make -B all
file kernel-rv kernel-la
readelf -h kernel-rv | grep -i flags
cd kernel && cargo fmt --check
```

Result:

- build exited 0
- `kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI`
- `Flags: 0x1, RVC, soft-float ABI`
- `cargo fmt --check` completed; only the existing Cargo config warning printed

## Reach evidence

### RV musl, cap760, before final scratch-umount addition

Log: `.codex-logs/scratch-cleanup-rv-musl-ltp-cap760-before-umount-20260616.log`

Result:

```text
initproc ltp: attempted 760 of 2820 cases, 110 returned 0
#### OS COMP TEST GROUP END ltp-musl ####
bad signal: none
```

This crosses the old 700-730 ramfs panic wall. `abort01` no longer stalls the
entire group; it exits through the watchdog as `124`, so it is not counted as
returned-0.

### RV musl, clean-build binary after final scratch-umount addition

Log: `.codex-logs/scratch-cleanup-rv-musl-ltp-cap760-20260616.log`

Stopped early to avoid spending the turn on known timeout-heavy regions:

```text
RUN=189 returned0=32 timeouts=14
last=RUN LTP CASE cp_tests.sh
bad signal: none
```

This confirms the final cleanup patch boots, survives `abort01`, passes the
cgroup timeout cluster, and shows no ramfs panic/page error in the early reach
path. It was not run to 740 again after the final small umount cleanup.

### Other quadrants

Short reach attempts after the first cleanup showed no ramfs panic before
interruption, but did not reach 730 in this turn:

- LA glibc: `.codex-logs/scratch-cleanup-la-glibc-ltp-cap760-20260616.log`
  - interrupted at `RUN=115`, `returned0=14`, `timeouts=10`
  - stopped in cgroup regression timeout cluster, bad signal empty
- LA musl: `.codex-logs/scratch-cleanup-la-musl-ltp-cap760-20260616.log`
  - old run hit harness stall at cgroup regression around `RUN=111`
- RV glibc: `.codex-logs/scratch-cleanup-rv-glibc-ltp-cap760-20260616.log`
  - old run reached `RUN=336`, bad signal empty

Remaining reach work is therefore throughput / cgroup-script stall handling for
the non-RV-musl quadrants, not a demonstrated recurrence of
`can't alloc frame in ram fs` in this patch.

## Basic-4 gate

Logs:

- `.codex-logs/basic-rv-ramfs-reach-umount-20260616.log`
- `.codex-logs/basic-la-ramfs-reach-umount-20260616.log`

Summary extracted from logs:

```text
[rv]
basic-glibc: start=True end=True code=0
basic-musl:  start=True end=True code=0
bad_count=0

[la]
basic-glibc: start=True end=True code=0
basic-musl:  start=True end=True code=0
bad_count=0
```

Note: the harness continued into later groups despite `EXPECTED_GROUPS`; those
processes were stopped after both basic groups had already emitted END and code
0. The basic-4 gate itself is green.

## Remaining work

- Run fresh cap740/cap760 for RV glibc, LA glibc, and LA musl after adding a
  focused workaround for cgroup regression / shell-script timeout clusters.
- Then perform the next full LTP pass and cluster returned failures by root
  cause: missing syscalls, missing `/proc`, device nodes, mount/overlay,
  permission semantics, etc.
