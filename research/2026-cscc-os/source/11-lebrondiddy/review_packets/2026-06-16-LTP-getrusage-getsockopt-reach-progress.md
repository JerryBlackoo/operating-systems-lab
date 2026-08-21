# LTP getrusage / getsockopt Reach Progress

Date: 2026-06-16 CST.

## Scope

This packet covers LA `ltp-glibc` stage-one reach work. The goal was to remove
the old `getrusage02` stall and the next `getsockopt01` kernel panic without
weakening the LTP walker or counting garbage output as success.

No OJ submission was made.

## Root Causes Addressed

- `getrusage()` accepted invalid `who` values and directly wrote through user
  pointers. `getrusage02` expected `EINVAL` for `who=-2`; the bad-pointer path
  then stalled the harness.
- `getsockopt()` read `optlen` and wrote `optval` through `UserRef::read/write`
  without fd/socket/user-pointer validation. `getsockopt01` bad-pointer cases
  triggered `UserRef::read().unwrap()` panic.
- `getsockopt01` also expects Linux errno semantics for invalid level/option
  combinations: `EOPNOTSUPP`, `ENOPROTOOPT`, and `EINVAL`.

## Changes

- `sys_getrusage()` now:
  - accepts `RUSAGE_SELF`, `RUSAGE_CHILDREN`, and `RUSAGE_THREAD`;
  - returns `EINVAL` for other `who` values;
  - checks the writable span used by LTP's guarded-buffer case before writing
    `utime/stime`.
- `sys_getsockopt()` now:
  - validates fd and socket type (`EBADF`, `ENOTSOCK`);
  - reads/writes user memory through checked helpers (`EFAULT`);
  - returns expected errno for unsupported level/option combinations;
  - preserves simple values for existing supported socket buffer options.

## Baseline

Clean LA glibc LTP-only run from a fresh sdcard image:

```text
Log: .codex-logs/la-glibc-ltp-clean-6600-20260616.log
RUN=856 returned0=147 timeouts=66
last=RUN LTP CASE getrusage02
getrandom01: FAIL LTP CASE getrandom01 : 0
getrusage01: FAIL LTP CASE getrusage01 : 0
stop reason: HARNESS STALL: no console output for 600s
```

`getrusage02` output before the stall included:

```text
getrusage02.c:64: TFAIL: getrusage(-2, ...) succeeded
```

## Fresh Verification

All commands ran on `lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the
official `zhouzhouyi/os-contest:20260510` image unless noted.

### Clean Build And Soft-Float

Log:

```text
.codex-logs/clean-build-getrusage-getsockopt-final-20260616.log
```

Result:

```text
cargo fmt --manifest-path kernel/Cargo.toml --all -- --check: exit code 0
rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; make -B all: exit code 0
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

Logs:

```text
.codex-logs/basic4-rv-getrusage-getsockopt-final-20260616.log
.codex-logs/basic4-la-getrusage-getsockopt-final-20260616.log
```

Results:

```text
RISC-V basic-glibc: START=1 END=1, finished in 2s (exited, code 0)
RISC-V basic-musl:  START=1 END=1, finished in 1s (exited, code 0)
LA basic-glibc:     START=1 END=1, finished in 2s (exited, code 0)
LA basic-musl:      START=1 END=1, finished in 2s (exited, code 0)
```

The full images continued into later groups after the basic markers; those later
group timeouts are not part of the basic-4 claim.

## Targeted Evidence

### getrusage

Targeted image linked these cases first and set `.ltp_case_limit=5`:

```text
000_getrusage01
001_getrusage02
002_getrusage03
003_getrusage03_child
004_getrusage04
```

Log:

```text
.codex-logs/la-glibc-getrusage-target-final5-20260616.log
```

Result:

```text
RUN=5 returned0=3
000_getrusage01: FAIL LTP CASE 000_getrusage01 : 0
001_getrusage02: FAIL LTP CASE 001_getrusage02 : 0
004_getrusage04: FAIL LTP CASE 004_getrusage04 : 0
```

`getrusage03` and `getrusage03_child` still fail/brok for unrelated child
accounting behavior.

### getsockopt01

The first RED targeted run confirmed the panic was gone but the errno matrix was
incomplete:

```text
Log: .codex-logs/la-glibc-getsockopt01-target-20260616.log
RUN=1 returned0=0
passed=3 failed=6 broken=0 skipped=0 warnings=0
```

After completing the socket option error paths:

```text
Log: .codex-logs/la-glibc-getsockopt01-target-final-20260616.log
RUN=1 returned0=1
passed=9 failed=0 broken=0 skipped=0 warnings=0
FAIL LTP CASE 000_getsockopt01 : 0
initproc ltp: attempted 1 of 2841 cases, 1 returned 0
```

Observed errno matrix:

```text
bad file descriptor: EBADF
non-socket fd: ENOTSOCK
invalid option buffer: EFAULT
invalid optlen pointer: EFAULT
invalid level: EOPNOTSUPP
unsupported UDP option: EOPNOTSUPP
invalid IP option: ENOPROTOOPT
invalid TCP option: ENOPROTOOPT
negative optlen: EINVAL
```

### creat05 Reach Probe

The final full walk below stopped before reaching `getrusage`, so `creat05` was
run as a targeted probe to check whether the case is intrinsically broken.

```text
Log: .codex-logs/la-glibc-creat05-target-20260616.log
RUN=1 returned0=1
FAIL LTP CASE 000_creat05 : 0
initproc ltp: attempted 1 of 2841 cases, 1 returned 0
```

This suggests the new `creat05` stall is interaction/history dependent in the
full walker, not a simple single-case `creat05` semantic failure.

## Full-Walk Evidence

After the `getrusage()` fix but before the `getsockopt()` fix:

```text
Log: .codex-logs/la-glibc-ltp-clean-6600-getrusage-final-20260616.log
RUN=863 returned0=149 timeouts=66
getrandom01:  FAIL LTP CASE getrandom01 : 0
getrusage01: FAIL LTP CASE getrusage01 : 0
getrusage02: FAIL LTP CASE getrusage02 : 0
getrusage04: FAIL LTP CASE getrusage04 : 0
last=RUN LTP CASE getsockopt01
stop reason: panic: called `Option::unwrap()` on a `None` value
```

Delta against the LA glibc clean baseline:

```text
attempted: 856 -> 863
returned0: 147 -> 149
old getrusage02 stall removed
new blocker exposed: getsockopt01 panic
```

After the `getsockopt()` fix, a fresh LA glibc LTP-only 6600s full walk was
started:

```text
Log: .codex-logs/la-glibc-ltp-clean-6600-getrusage-getsockopt-final-20260616.log
RUN=245 returned0=34 timeouts=36
last=RUN LTP CASE creat05
stop reason: HARNESS STALL: no console output for 600s
```

This run did not reach `getrusage02` or `getsockopt01`, so it is not counted as
post-getsockopt full-delta proof. It does record a new earlier full-walk reach
blocker: the walker stalls after `creat05` prints a clean LTP summary.

## Current Next Blocker

The next stage-one reach blocker to investigate is:

```text
full LA glibc walk stalls after creat05 summary, before FAIL LTP CASE creat05
```

The targeted `creat05` probe returns 0, so the likely root is not the single
`creat05` syscall expectation. It is more likely an accumulated full-walk state
or fd/resource cleanup interaction after the case opens up to the fd-table
limit.

## Status

- `getrusage02` old LA glibc full-walk blocker: removed.
- `getsockopt01` panic: removed; targeted case now truly returns 0.
- Full post-getsockopt depth: blocked by earlier `creat05` full-walk stall and
  needs a separate reach-debugging pass.
- No OJ submission was made.
