# 2026-06-16 LTP reach quarantine2 results

## Scope

This packet records the current P1 reach verification after the ramfs scratch
cleanup and the temporary reach quarantines for known crash-storm cases.

It does not count quarantined cases as returned-0. The only score-like count
used below is the runner summary `attempted ... returned 0` / `FAIL LTP CASE
... : 0`.

## Changes under validation

- LTP case scratch cleanup after every case:
  - clear `/tmp`, `/dev/shm`, `/var/tmp`, `/var/lock`, `/run`
  - clear `<libc>/tmp` and `<libc>/mntpoint`
  - try `umount()` before removing scratch children
- Mount replacement cleanup:
  - `mount_fs()` replaces an existing mounted path instead of keeping stale
    ramfs roots reachable.
- ramfs allocation failure handling:
  - ramfs `writeat()` / `truncate()` return `ENOSPC` instead of panicking on
    `can't alloc frame in ram fs`.
- Reach quarantines:
  - existing crash-storm cases remain quarantined.
  - `mallocstress` and `gre01.sh` are quarantined as reach stopgaps.
  - These print `FAIL LTP CASE ... : 124`; they are not counted as passed.
- RISC-V trap layer:
  - high-address `LoadFault` / `LoadMisaligned` is no longer converted into an
    immediate low-level `panic!("kernel error")`; it is surfaced as
    `TrapType::LoadPageFault`.

## Build / ABI / formatting gate

Log:

```text
.codex-logs/clean-build-ltp-reach-quarantine2-20260616.log
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
cargo fmt --check: completed, only existing Cargo config warning
```

## Basic-4 gate

The earlier full-image basic run emitted both basic END markers but the outer
process timed out after continuing into later groups, so it was not counted as
the gate.

Fresh basic-only images with explicit `basic-glibc` / `basic-musl` markers were
used for the real gate.

Logs:

```text
.codex-logs/basic4-rv-ltp-reach-quarantine2-basic-only-marked-20260616.log
.codex-logs/basic4-la-ltp-reach-quarantine2-basic-only-marked-20260616.log
.codex-logs/basic4-ltp-reach-quarantine2-basic-only-marked-status-20260616.txt
```

Result:

```text
basic_only_marked_rv_status=0
basic_only_marked_la_status=0
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Baseline being compared against

From `.codex-logs/ltp-baseline-*-6600-20260616.summary`:

```text
la-glibc: RUN=878  returned0=153; stopped at gre01.sh; bad=LoadPageFault panic
la-musl:  RUN=2820 returned0=173; full walk END present
rv-glibc: RUN=1177 returned0=173; stopped at mallocstress; bad=kernel error panic
rv-musl:  RUN=1157 returned0=162; stopped at mallocstress; bad=kernel error panic
```

The old `~700-730` ramfs exhaustion wall is already passed in this baseline;
the current walls are post-730 gre/mallocstress reach failures and throughput.

## Targeted wall-sequence evidence

### LA glibc: gre01 quarantine lets the walker continue

Log:

```text
.codex-logs/ltp-quarantine2-seq-la-glibc-gre01.sh-20260616.log
```

Result:

```text
RUN LTP CASE 000_gre01.sh
initproc watchdog: quarantined LTP case 000_gre01.sh before known unrecoverable crash storm
FAIL LTP CASE 000_gre01.sh : 124
RUN LTP CASE abort01
FAIL LTP CASE abort01 : 1
RUN LTP CASE abs01
FAIL LTP CASE abs01 : 1
RUN LTP CASE accept01
FAIL LTP CASE accept01 : 124
RUN LTP CASE accept02
FAIL LTP CASE accept02 : 2
initproc ltp: attempted 5 of 2841 cases, 0 returned 0
#### OS COMP TEST GROUP END ltp-glibc ####
```

### RV glibc: mallocstress quarantine lets following cases return 0

Log:

```text
.codex-logs/ltp-quarantine2-seq-rv-glibc-mallocstress-20260616.log
```

Result:

```text
RUN LTP CASE 000_mallocstress
initproc watchdog: quarantined LTP case 000_mallocstress before known unrecoverable crash storm
FAIL LTP CASE 000_mallocstress : 124
RUN LTP CASE 001_mallinfo01
FAIL LTP CASE 001_mallinfo01 : 0
RUN LTP CASE 002_mallinfo02
FAIL LTP CASE 002_mallinfo02 : 0
RUN LTP CASE 003_mallinfo2_01
FAIL LTP CASE 003_mallinfo2_01 : 0
RUN LTP CASE 004_madvise01
FAIL LTP CASE 004_madvise01 : 2
initproc ltp: attempted 5 of 2845 cases, 3 returned 0
#### OS COMP TEST GROUP END ltp-glibc ####
```

### RV musl: mallocstress quarantine also keeps the walker alive

Log:

```text
.codex-logs/ltp-quarantine2-seq-rv-musl-mallocstress-20260616.log
```

Result:

```text
RUN LTP CASE 000_mallocstress
initproc watchdog: quarantined LTP case 000_mallocstress before known unrecoverable crash storm
FAIL LTP CASE 000_mallocstress : 124
RUN LTP CASE 001_mallinfo01
FAIL LTP CASE 001_mallinfo01 : 32
RUN LTP CASE 002_mallinfo02
FAIL LTP CASE 002_mallinfo02 : 32
RUN LTP CASE 003_mallinfo2_01
FAIL LTP CASE 003_mallinfo2_01 : 32
RUN LTP CASE 004_madvise01
FAIL LTP CASE 004_madvise01 : 1
initproc ltp: attempted 5 of 2825 cases, 0 returned 0
#### OS COMP TEST GROUP END ltp-musl ####
```

## Fresh reach windows

All runs used fresh sdcard images with only the target libc LTP script enabled,
plus a case-count cap to avoid spending a full judge window while still crossing
the known wall.

### RV glibc

Log:

```text
.codex-logs/ltp-quarantine2-rv-glibc-cap1230-20260616.log
.codex-logs/ltp-quarantine2-rv-glibc-cap1230-20260616.summary
```

Result:

```text
RUN=1230 returned0=177 timeouts=82
summary=initproc ltp: attempted 1230 of 2840 cases, 177 returned 0
end=#### OS COMP TEST GROUP END ltp-glibc ####
bad=
```

Delta against baseline:

```text
attempted: 1177 -> 1230
returned0: 173  -> 177
```

This crosses the old `mallocstress` panic wall and produces a small true
returned-0 gain. It is not a huge batch yet, but it proves the reach blocker is
no longer killing the RV glibc walk at `mallocstress`.

### RV musl

Log:

```text
.codex-logs/ltp-quarantine2-rv-musl-cap1230-20260616.log
.codex-logs/ltp-quarantine2-rv-musl-cap1230-20260616.summary
```

Result:

```text
RUN=1230 returned0=165 timeouts=97
summary=initproc ltp: attempted 1230 of 2820 cases, 165 returned 0
end=#### OS COMP TEST GROUP END ltp-musl ####
bad=
```

Delta against baseline:

```text
attempted: 1157 -> 1230
returned0: 162  -> 165
```

This also crosses the old `mallocstress` panic wall and gains true returned-0.

### LA glibc

Log:

```text
.codex-logs/ltp-quarantine2-la-glibc-cap930-20260616.log
.codex-logs/ltp-quarantine2-la-glibc-cap930-20260616.summary
```

Result:

```text
RUN=879 returned0=153 timeouts=72
last=RUN LTP CASE gre02.sh
last_fail=FAIL LTP CASE gre01.sh : 124
bad=[Core 0] panic: 'page fault: LoadPageFault(
```

Context:

```text
RUN LTP CASE gre01.sh
initproc watchdog: quarantined LTP case gre01.sh before known unrecoverable crash storm
FAIL LTP CASE gre01.sh : 124
RUN LTP CASE gre02.sh
sh: syntax error: unterminated '
sh: syntax error: unexpected )
[Core 0] panic: 'page fault: LoadPageFault(0x1201bdc58)'
```

Delta against baseline:

```text
attempted: 878 -> 879
returned0: 153 -> 153
```

Read: `gre01.sh` is not an isolated case. `gre02.sh` is the second member of the
same `gre*.sh` family and hits the same LoongArch page-fault class. Per the
escalation rule, this should not be chased with another blind single-case patch.

## Current conclusion

- The ramfs `can't alloc frame in ram fs` wall is not reproduced in the current
  validation. Current fresh windows pass 730 cases where relevant.
- RV glibc and RV musl both cross the old `mallocstress` wall and end cleanly at
  cap1230 with true returned-0 growth.
- LA musl already had a full 2820/2820 baseline walk and was not rerun here.
- LA glibc remains blocked by a `gre*.sh` LoongArch page-fault family:
  `gre01.sh` quarantine moves the wall to `gre02.sh`, but the underlying root
  cause is still unresolved.

## Next action

Escalate or decide explicitly between:

1. root-fixing the LoongArch user fault / shell-script failure path so `gre*.sh`
   exits as a case failure instead of panicking the kernel, or
2. quarantining the small `gre*.sh` family as a reach-only policy and then
   running a new LA glibc reach window.

Do not count the quarantine as score. Do not continue adding one `gre` case at a
time.
