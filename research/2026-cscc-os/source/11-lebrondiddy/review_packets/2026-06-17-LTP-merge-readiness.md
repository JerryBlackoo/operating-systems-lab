# 2026-06-17 LTP merge-readiness report

## Scope

M3 checkpoint for `wip/ltp-reach-20260616`.

Goal:

- commit the leftover housekeeping packet before testing;
- run a clean official-image build and ABI check;
- gate basic-4;
- run judge-budget LTP in all four architecture/libc quadrants;
- decide whether this WIP is ready to merge to `main`.

No new kernel fixes were made during this checkpoint.

## Branch / commit context

Local branch under test:

```text
wip/ltp-reach-20260616
```

Housekeeping commit made before gates:

```text
f50dc45 docs: record ltp milestone housekeeping
```

The 16 LTP code commits in this milestone, excluding the prior ungated reach
commit and docs-only commits:

```text
5baac57 LTP batch: fix fallocate and rmdir cleanup
48a21c6 LTP batch: add fdatasync fd validation
730951a LTP batch: add path truncate syscall
20f88e9 LTP batch: fix fstatat statx lookup flags
6446101 LTP batch: fix fcntl dupfd and setfl flags
942b498 LTP batch: validate fchownat lookup flags
26c59a4 LTP batch: add fd-based chdir
a51995c LTP batch: add getpriority syscall
d955936 LTP batch: add getsid syscall
be690a3 LTP batch: reject non-empty ramfs rmdir
48d596b LTP batch: handle getpgid pid cases
2d295d8 LTP batch: persist ramfs chown metadata
1829a48 LTP batch: validate lchown errno paths
5117f91 LTP batch: enforce fchdir search permission
baa82a9 LTP batch: fix truncate03 errno paths
6939a3e LTP batch: fix fcntl07 exec cloexec
```

## Clean build / ABI gate

Evidence:

```text
.codex-logs/m3-clean-build-20260617.log
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Clean build passed.

## Basic-4 gate

Basic gate used disposable marked basic-only images, not the full OJ image. This
avoids continuing into later groups after the basic markers.

Evidence:

```text
.codex-logs/m3-basic-marked-rv-20260617.log
.codex-logs/m3-basic-marked-la-20260617.log
```

Status:

| Quadrant | Status | Evidence |
| --- | --- | --- |
| rv-glibc | PASS | `group basic-glibc finished in 0s (exited, code 0)` |
| rv-musl | PASS | `group basic-musl finished in 0s (exited, code 0)` |
| la-glibc | PASS | `group basic-glibc finished in 1s (exited, code 0)` |
| la-musl | PASS | `group basic-musl finished in 1s (exited, code 0)` |

Harness also reported:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

Basic-4 is green.

## LTP full-run method

Judge-budget LTP was run with:

```text
TIMEOUT_SECONDS=6600
STALL_TIMEOUT_SECONDS=600
```

Each quadrant used a fresh image extracted from the official sdcard `.xz`, then
trimmed so that the guest saw exactly one `*_testcode.sh`: the target
`ltp_testcode.sh`. The runner also removed `.ltp_case_limit` files and wrote
`/.watchdog_global_deadline_secs = 6600`.

Persistent log evidence shows each run started directly in the target LTP group
and did not emit earlier non-LTP group START markers:

```text
.codex-logs/m3-ltp-rv-glibc-6600-20260617.log: START ltp-glibc
.codex-logs/m3-ltp-rv-musl-6600-20260617.log:  START ltp-musl, END ltp-musl
.codex-logs/m3-ltp-la-glibc-6600-20260617.log: START ltp-glibc
.codex-logs/m3-ltp-la-musl-6600-20260617.log:  START ltp-musl, END ltp-musl
```

Counting rule: count only real LTP case exit code 0 via `FAIL LTP CASE ... : 0`
and the guest summary `returned 0`. Do not count TBROK/generic garbage.

## LTP result

Baseline is the 2026-06-16 judge-budget packet:

```text
review_packets/2026-06-16-LTP-baseline-6600.md
```

Current evidence:

```text
.codex-logs/m3-ltp-rv-glibc-6600-20260617.log
.codex-logs/m3-ltp-rv-musl-6600-20260617.log
.codex-logs/m3-ltp-la-glibc-6600-20260617.log
.codex-logs/m3-ltp-la-musl-6600-20260617.log
```

| Quadrant | Baseline attempted / returned0 | Current attempted / returned0 | Delta | Terminal state |
| --- | ---: | ---: | ---: | --- |
| rv-glibc | 1177 / 173 | 370 / 83 | -90 | kernel panic at `execve03` |
| rv-musl | 1157 / 162 | 2820 / 217 | +55 | full walk, END marker present |
| la-glibc | 878 / 153 | 370 / 79 | -74 | kernel panic at `execve03` |
| la-musl | 2820 / 173 | 2820 / 226 | +53 | full walk, END marker present |

The focused `+38 returned0` claim is confirmed in the full `la-musl` walk:

```text
la-musl: 173 -> 226 returned0, delta +53
```

The same batch also improved `rv-musl` in the full walk:

```text
rv-musl: 162 -> 217 returned0, delta +55
```

But the four-quadrant aggregate is not a net gain:

```text
total returned0: 661 -> 605, delta -56
```

## Regression / blocker

Both glibc quadrants now hit the same kernel panic in `execve03` before the old
reach walls:

```text
RUN LTP CASE execve03
execve03.c:128: TFAIL: execve failed unexpectedly; expected File name too long: ENOENT (2)
execve03.c:124: TPASS: execve failed as expected: ENOENT (2)
execve03.c:128: TFAIL: execve failed unexpectedly; expected Not a directory: ENOENT (2)
execve03.c:124: TPASS: execve failed as expected: EFAULT (14)
[Core 0] [kernel/src/tasks/exec.rs:218]
[Core 0] panic: 'index out of bounds: the len is 0 but the index is 0'
```

This is present in:

```text
.codex-logs/m3-ltp-rv-glibc-6600-20260617.log
.codex-logs/m3-ltp-la-glibc-6600-20260617.log
```

The musl runs exit the guest LTP group cleanly with full END markers, but the
outer harness returns status 5 because `check_basic_output()` greps generic
failure strings such as `unknown command`. That harness status is not counted as
a kernel panic; the guest summaries are still valid returned0 evidence.

## Merge recommendation

Recommendation: **do not merge to `main` as-is**.

Reason:

- basic-4 is green;
- `la-musl` and `rv-musl` full LTP returned0 improved substantially;
- however, `rv-glibc` and `la-glibc` now regress to an earlier hard kernel panic
  at `execve03`;
- aggregate four-quadrant returned0 is negative versus the 2026-06-16 baseline
  (`661 -> 605`, delta `-56`).

M3 decision: **HOLD / NO-GO for main**. The WIP branch has useful musl gains, but
it should not be merged until the `execve03` panic is explained and fixed, then
the four-quadrant LTP measurement is rerun.

Actual merge to `main`, push, and OJ submission remain human/Claude decisions.
