# Review Packet: net front-door score sprint

Date: 2026-06-21 CST
Prepared by: Codex

## Current Goal

Independent scoring sprint from `main@0984936` on branch
`wip/net-frontdoor-score-20260620`, targeting iperf/netperf front-door failures
without touching the active `wip/time-nanosleep-overflow-20260620` evaluation
branch.

## Root Cause

Two small Linux-compatibility gaps were blocking network test front doors before
the network stack itself:

- `/dev/null` had read semantics for `/dev/zero`: it filled the buffer and
  returned the requested length. Linux `/dev/null` reads return EOF (`0`) and
  leave the caller buffer irrelevant.
- `statx` encoded `stx_rdev_minor = stat.rdev` and `stx_rdev_major = 0`.
  Daemon implementations validate `/dev/null` as char device major/minor
  `1:3`; LoongArch glibc reached `statx` and rejected the old split as
  `ENODEV`.

The remaining iperf/netperf failures after this patch are later socket/watchdog
or dynamic-library issues, not the original `/dev/null` front-door failure.

## Key Diff

- `kernel/filesystem/devfs/src/null.rs`
  - `/dev/null` `readat` returns `Ok(0)`.
  - `writeat` continues returning the full input length.
  - `stat.rdev = 0x103`, matching Linux null device major/minor `1:3`.
  - Added a unit test for read/write/rdev semantics.
- `kernel/filesystem/devfs/src/lib.rs`
  - Added test-only `extern crate std`.
  - Added a test-only `Tty` stub so `devfs` unit tests do not link through the
    kernel runtime allocator path.
- `kernel/kernel/src/syscall/fd.rs`
  - Added Linux-compatible major/minor extraction.
  - `stat_to_statx()` now reports major/minor for both `rdev` and `dev`.

No LTP runner, harness, fake `returned0`, cancel-signal-futex, or network stack
logic was changed.

## RED Evidence

Artifact root:
`/home/yxzhang/oskernel-artifacts/net-frontdoor-20260620`

- `red-rv-iperf-glibc.log`: `iperf3` failed early with
  `unable to become a daemon: No such device`.
- `red-la-iperf-glibc.log`: same front-door daemon failure.
- `red-rv-netperf-musl.log`: already reached a UDP throughput table, proving
  some network output was possible once past setup.
- `red-la-netperf-musl.log`: `netserver` started, then hit later watchdog.
- `devfs-null-red3.log`: after fixing the unit-test harness, the old
  `/dev/null` read behavior failed the focused test with `left: Ok(8)`,
  `right: Ok(0)`.
- `statx-rdev-red.log` and `stat-trace-la-iperf-glibc.log`: diagnostic evidence
  that LoongArch glibc daemon validation used `statx` and rejected the old
  `/dev/null` major/minor encoding.

Temporary syscall traces were removed before final diff review.

## GREEN Targeted Network Evidence

Final focused runs used fresh disposable 4G copies of the official staged
images, cropped to one network group/libc quadrant each.

All final network logs below have:

- `unable to become a daemon`: `0`
- `No such device`: `0`
- `/dev/null debug file`: `0`
- harness return code: `0`

Logs:

- `green-final-rv-iperf-glibc.log`: daemon front door cleared; now reaches
  later syscall 72/207 watchdog and connection refused output.
- `green-final-rv-iperf-musl.log`: same later watchdog class.
- `green-final-rv-netperf-glibc.log`: blocked by known
  `GLIBC_PRIVATE`/`libm.so.6` mismatch, unrelated to this patch.
- `green-final-rv-netperf-musl.log`: prints a real netperf throughput table,
  including `10^6bits/sec`, before later watchdog cleanup.
- `green-final-la-iperf-glibc.log`: daemon `ENODEV` is gone; later syscall
  72/207 watchdog and connection refused output remain.
- `green-final-la-iperf-musl.log`: reaches a real iperf connection line
  (`connected to 10.0.2.15 port 5001`), then fails later on
  `getsockopt - Protocol not available` / watchdog.
- `green-final-la-netperf-glibc.log`: blocked by known
  `GLIBC_PRIVATE`/`libm.so.6` mismatch.
- `green-final-la-netperf-musl.log`: `netserver` starts without the old
  `/dev/null` error; later syscall 72 watchdog prevents control connection.

QEMU caveat for all remote runs: Docker image currently provides QEMU `10.0.2`,
while contest README records `9.2.1`.

## Required Gates

Final gates were run in an isolated clean remote worktree:
`/home/yxzhang/oskernel2025-x-netfrontdoor-gate`

The clean worktree was created from `0984936bc4f22946fb54d2cf442a1f08cfdd14ec`
and had only these modified files:

- `kernel/filesystem/devfs/src/lib.rs`
- `kernel/filesystem/devfs/src/null.rs`
- `kernel/kernel/src/syscall/fd.rs`

Gate logs:
`/home/yxzhang/oskernel-artifacts/net-frontdoor-20260620/final-gates`

- `cargo-fmt-check.log`: `cargo fmt --all -- --check` passed.
- `clean-build-file-readelf.log`: clean dual-arch build passed:
  - `kernel-rv`: `ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI`
  - `kernel-la`: `ELF 64-bit LSB executable, LoongArch`
  - `readelf -h kernel-rv`: `Flags: 0x1, RVC, soft-float ABI`
- `basic-rv-strict.log`: strict `basic-glibc,basic-musl` gate passed.
- `basic-la-strict.log`: strict `basic-glibc,basic-musl` gate passed.
- `busybox-musl-rv.log`: `55` busybox successes, `0` failures.
- `busybox-musl-la.log`: `54` busybox successes, `1` known failure:
  `sh -c 'sleep 5' & ./busybox kill $!`.

Local Mac `cargo test` was not runnable because `cargo` is not on the local
shell `PATH`; the focused devfs unit test evidence is from the remote Docker
logs above.

## Recommendation

This patch is safe to merge to `main`: it is a narrow Linux-compatibility fix,
passes clean build/basic/busybox gates, and removes the iperf/netperf
`/dev/null` front-door failures without touching broader scoring harnesses.

After merge, `main` remains the OJ1508 fallback line plus this net-frontdoor
small fix; `wip/time-nanosleep-overflow-20260620` remains unmerged.

An OJ diagnostic submission is reasonable if the team wants to test whether
the RV netperf-musl throughput and LA iperf-musl connection progress produce
any score delta. It is not expected to fully solve network rows because
post-front-door socket/watchdog failures remain.

OJ submission remains human-only.

## Files Claude Should Read

- `kernel/filesystem/devfs/src/null.rs`
- `kernel/filesystem/devfs/src/lib.rs`
- `kernel/kernel/src/syscall/fd.rs`
- This packet
