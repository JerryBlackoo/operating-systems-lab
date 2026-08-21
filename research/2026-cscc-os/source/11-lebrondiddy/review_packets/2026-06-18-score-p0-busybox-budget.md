# 2026-06-18 score-p0 busybox budget fix

Date: 2026-06-18
Branch: `wip/score-p0-20260618`
Base: `main/a0fff71`

## Scope

Score-first P0 pass over `libctest-glibc` and `busybox-musl` before entering
the larger LA-LTP long-tail work.

No OJ submission was made.

## Build

Clean build command:

```sh
rm -rf kernel/target kernel-rv kernel-la kernel/mount.img
make -B all
file kernel-rv kernel-la
readelf -h kernel-rv | grep -i flags
```

Evidence:

```text
/home/yxzhang/oskernel-artifacts/score-p0-20260618/clean-build-busybox-budget-20260618-235257.log
```

Observed:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

## A. libctest-glibc probe

`main/a0fff71` skips `libctest-glibc` in `SKIPPED_MARKER_GROUPS`, so the probe
used a detached diagnostic worktree under the artifact directory with only this
temporary diff:

```diff
-const SKIPPED_MARKER_GROUPS: [&str; 4] = [
-    "libctest-glibc",
+const SKIPPED_MARKER_GROUPS: [&str; 3] = [
```

This diagnostic diff was not committed.

Images were fresh disposable copies of the staged 4G images, pruned to keep only
the `/glibc/libctest_testcode.sh` entry point. This is targeted diagnostic
evidence, not full/OJ-like evidence.

Logs:

```text
/home/yxzhang/oskernel-artifacts/score-p0-20260618/rv-libctest-glibc-noskip-20260618-234339.log
/home/yxzhang/oskernel-artifacts/score-p0-20260618/la-libctest-glibc-noskip-20260618-234339.log
```

Result:

| Arch | START/END | Time | `Pass!` | Failure/stall |
| --- | --- | ---: | ---: | --- |
| RV | yes | `41s` | `0` | no panic/page fault/HARNESS STALL |
| LA | yes | `41s` | `0` | no panic/page fault/HARNESS STALL |

Conclusion: opening `libctest-glibc` is not a score fix in this branch. It
finishes safely, but it produces no real `Pass!` lines because the glibc dynamic
path fails on loader/library symbol-version errors and static tests still fail.
The production skip remains unchanged.

## B. busybox-musl reproduction

The OJ symptom was `busybox-musl = 10`. Fresh 4G targeted local probes reproduced
that exact cutoff on both RV and LA.

Before-fix logs:

```text
/home/yxzhang/oskernel-artifacts/score-p0-20260618/rv-busybox-musl-target-20260618-234643.log
/home/yxzhang/oskernel-artifacts/score-p0-20260618/la-busybox-musl-target-20260618-234643.log
```

Before-fix counters:

| Arch | Success | Fail | Terminal signal |
| --- | ---: | ---: | --- |
| RV | `10` | `0` | `kernel watchdog: task 5 syscall 260 exceeded wall-clock limit` |
| LA | `10` | `0` | `kernel watchdog: task 5 syscall 260 exceeded wall-clock limit` |

The full staged image's `/musl/busybox_cmd.txt` contains 55 commands. The
guest loop stopped during command 11, `du`, because the BusyBox per-child task
budget was `10s`, while `du` over the full 4G payload tree needs more than that.
The enclosing busybox group budget was already `120s`; only the child-task cap
was too small for the official full image.

## Fix

Change only BusyBox per-child task budget from `10s` to `60s`, leaving:

- BusyBox group budget at `120s`;
- Lua per-child budget at `10s`;
- LTP per-case budget at `15s`;
- libctest skip behavior unchanged.

## B. busybox-musl after

After-fix logs:

```text
/home/yxzhang/oskernel-artifacts/score-p0-20260618/rv-busybox-musl-after-20260618-235446.log
/home/yxzhang/oskernel-artifacts/score-p0-20260618/la-busybox-musl-after-20260618-235446.log
```

After-fix counters:

| Arch | Success | Fail | Time | Watchdog kill |
| --- | ---: | ---: | ---: | --- |
| RV | `55` | `0` | `26s` | no |
| LA | `54` | `1` | `50s` | no |

The remaining LA failure is the historical line 23:
`sh -c 'sleep 5' & ./busybox kill $!`. It is not the OJ=10 cutoff root cause,
and the rest of the command list now runs.

## Basic Gate

Valid basic quick gate:

```text
/home/yxzhang/oskernel-artifacts/score-p0-20260618/basic4-quick-generic-marker-busybox-budget-20260618-235853.log
```

Result:

```text
PASS: RISC-V basic markers and real basic case output found
RV_RC=0
PASS: LoongArch basic markers and real basic case output found
LA_RC=0
HOST_END ... rc=0
```

The first basic attempt,
`basic4-quick-busybox-budget-20260618-235740.log`, is invalid as a gate because
the quick images print generic `basic` markers while the command expected
`basic-glibc,basic-musl`. The guest basic cases did run, but the valid gate is
the generic-marker rerun above.

## Decision

This branch takes the successful path B:

- local targeted `busybox-musl` reproduces the OJ cutoff at 10;
- the kernel-side root cause is an overly small BusyBox per-child task budget
  for official full images;
- the patch raises only that cap;
- targeted `busybox-musl` improves to RV `55/55` and LA `54/55`;
- basic quick gate remains green.

No LA-LTP long-tail code change was attempted in this branch because A/B yielded
a small safe fix.
