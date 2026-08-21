# LTP Depth Budget / Reach Blockers

Date: 2026-06-16 CST.

## Scope

This packet covers the P1 LTP reach work for the "run deep, then batch by root
cause" loop. The work is still stage-one reach work, not a claim that all four
LTP quadrants are close to the full 2841-case walk.

No OJ submission was made.

## Changes

- LTP group budget now uses the global deadline instead of the old 900s local
  cap, so LTP can spend the judge-level window on the case walk.
- ramfs `writeat()` and `truncate()` now return `ENOSPC` on frame allocation
  failure instead of panicking. This removes the `ftruncate03` reach panic while
  leaving the semantic failure visible to LTP.
- user output writes were hardened for `getcwd`, `fstat`, `fstatat`, and
  `statx`, avoiding kernel hangs on bad user output pointers.
- `getsockname()` and `getpeername()` now return Linux-style errno for invalid
  fd, non-socket fd, unsupported/unconnected peer state, bad length pointers,
  and short/invalid lengths instead of panicking.
- `getrandom()` now validates flags, returns `EFAULT` for bad/null output
  buffers, and fills user memory through checked user writes instead of directly
  dereferencing the user pointer.

## Baseline

Baseline logs are the judge-level four-quadrant LTP runs with
`TIMEOUT_SECONDS=6600` and `STALL_TIMEOUT_SECONDS=600`:

```text
la-glibc: attempted 540/2840,  90 returned 0; stopped by old 900s LTP budget before flock04
la-musl:  attempted 705/2820, 113 returned 0; stopped by old 900s LTP budget before ftrace_stress_test.sh
rv-glibc: attempted 553/2840, 100 returned 0; stopped by old 900s LTP budget before fork_exec_loop
rv-musl:  attempted 705/2820, 103 returned 0; stopped by old 900s LTP budget before ftrace_stress_test.sh
```

## Fresh Verification

All commands ran on `lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the
official `zhouzhouyi/os-contest:20260510` image unless noted.

### Formatting

```text
cargo fmt --manifest-path kernel/Cargo.toml --all -- --check
exit code 0
```

### Clean Build And Soft-Float

Log:

```text
.codex-logs/clean-build-ltp-depth-getrandom-20260616.log
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

Logs:

```text
.codex-logs/basic4-rv-getrandom-20260616.log
.codex-logs/basic4-la-getrandom-20260616.log
```

Results:

```text
RISC-V:   basic-glibc START/END 1/1, basic-musl START/END 1/1
          group basic-glibc finished in 2s (exited, code 0)
          group basic-musl  finished in 1s (exited, code 0)

LoongArch: basic-glibc START/END 1/1, basic-musl START/END 1/1
           group basic-glibc finished in 2s (exited, code 0)
           group basic-musl  finished in 2s (exited, code 0)
```

The outer harness exit was 124 because the full image continued into later
groups after the basic markers. That later timeout is not part of the basic-4
claim.

## Targeted Reach Evidence

### ramfs / ftruncate

Targeted logs:

```text
.codex-logs/rv-musl-ftruncate-target-ramfs3-20260616.log
.codex-logs/la-glibc-ftruncate-target-ramfs3-20260616.log
```

Both targeted runs reached `ftruncate03` without the old
`panic: can't alloc frame in ram fs`. The case still reports semantic failure,
but the LTP walker continues.

### usercopy / getcwd and fstat

Targeted logs:

```text
.codex-logs/rv-glibc-fstat-getcwd-target-usercopy2-20260616.log
.codex-logs/rv-musl-getcwd-fstat-target-usercopy2-20260616.log
.codex-logs/la-glibc-getcwd-fstat-target-usercopy2-20260616.log
```

Observed:

```text
rv-glibc: fstat03 and getcwd01 returned 0; group end present
rv-musl:  targeted getcwd/fstat set finished; no stall
la-glibc: targeted getcwd/fstat set finished; no stall
```

### socket address queries

Targeted logs:

```text
.codex-logs/rv-glibc-getpeer-target-socketaddr-20260616.log
.codex-logs/la-glibc-getpeer-target-socketaddr-20260616.log
```

Observed:

```text
RUN=2 returned0=0
getpeername01 / getsockname01 still fail semantically, but group end is present
and the old "can't get socket address: Unsupported" panic is gone.
```

### getrandom01

`getrandom01` was the next common reach blocker after socketaddr. Before this
fix all four 2700s LTP windows reached `getrandom01` and then the harness
stalled:

```text
la-glibc: RUN=835 returned0=134, last=RUN LTP CASE getrandom01, HARNESS STALL 480s
la-musl:  RUN=815 returned0=156, last=RUN LTP CASE getrandom01, HARNESS STALL 480s
rv-glibc: RUN=835 returned0=132, last=RUN LTP CASE getrandom01, HARNESS STALL 480s
rv-musl:  RUN=815 returned0=145, last=RUN LTP CASE getrandom01, HARNESS STALL 480s
```

Single-case LTP walker logs after the fix:

```text
.codex-logs/la-glibc-getrandom01-only-getrandom-20260616.log
.codex-logs/la-musl-getrandom01-only-getrandom-20260616.log
.codex-logs/rv-glibc-getrandom01-only-getrandom-20260616.log
.codex-logs/rv-musl-getrandom01-only-getrandom-20260616.log
```

Results:

```text
la-glibc: RUN=1 returned0=1 TPASS=4 TFAIL=0 TBROK=0
la-musl:  RUN=1 returned0=1 TPASS=4 TFAIL=0 TBROK=0
rv-glibc: RUN=1 returned0=1 TPASS=4 TFAIL=0 TBROK=0
rv-musl:  RUN=1 returned0=1 TPASS=4 TFAIL=0 TBROK=0
```

Each run used an unmodified initproc LTP walker, a temporary image with only the
target libc's `ltp_testcode.sh`, `000_getrandom01` linked first, and
`.ltp_case_limit=1`.

## Depth Delta

The socketaddr-era 2700s four-quadrant window showed a real reach delta over the
baseline:

```text
la-glibc: 90  -> 134 returned0, attempted 540 -> 835
la-musl:  113 -> 156 returned0, attempted 705 -> 815
rv-glibc: 100 -> 132 returned0, attempted 553 -> 835
rv-musl:  103 -> 145 returned0, attempted 705 -> 815
```

After the `getrandom()` fix, a clean LA glibc LTP-only 2700s run from a freshly
decompressed sdcard image reached past `getrandom01`:

```text
Log: .codex-logs/la-glibc-getrandom-clean-ltp-only-2700-20260616.log
RUN=856 returned0=147
saw_getrandom01 FAIL LTP CASE getrandom01 : 0
last=RUN LTP CASE getrusage02
bad=HARNESS STALL: no console output for 480s
```

Delta against the preceding LA glibc 2700s window:

```text
la-glibc: 134 -> 147 returned0, attempted 835 -> 856
```

The other three quadrants have targeted `getrandom01` proof but still need a
fresh clean full-window LTP-only rerun to measure post-getrandom depth. Earlier
attempts using already-mutated full sdcard copies were not counted because LTP
modifies the image and produced misleading intermediate stalls.

## Current Next Blocker

The next observed clean LA glibc blocker is `getrusage02`:

```text
getrusage02.c:64: TFAIL: getrusage(-2, 0x...) succeeded
kernel watchdog: task ... syscall 165 exceeded wall-clock limit
HARNESS STALL: no console output for 480s
```

This likely belongs to the same user-output/errno-semantics class:

- `who = -2` should return `EINVAL`, not success.
- bad output pointers should return `EFAULT`.
- `sys_getrusage()` still writes through `UserRef::with_mut()`.

That is the next reach item; it was not folded into this packet to keep the
change scope clear.

## Notes

- The runner line `FAIL LTP CASE name : 0` is the fixed contest label; this
  packet counts only `: 0` as returned-0.
- Misconfigured full-image runs that spent time in basic/libcbench before LTP,
  or runs from mutated sdcard copies, are not used as scoring evidence.
- No QEMU or Docker process remained running after the final verification.
