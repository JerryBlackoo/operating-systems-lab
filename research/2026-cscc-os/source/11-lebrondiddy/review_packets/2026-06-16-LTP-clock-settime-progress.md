# LTP clock_settime Progress

Date: 2026-06-16 CST.

## Scope

This packet covers a minimal `clock_settime()` implementation for
`CLOCK_REALTIME`. The target symptom was LA LTP time cases that failed or
cleanup-broke because syscall 112 was missing and returned `ENOSYS`.

The honest scoring result from this follow-up is:

```text
LA ltp-glibc cap350: 105 returned 0 -> 109 returned 0
Delta: +4 returned-count cases
```

## Root Cause Addressed

`clock_settime` was not dispatched, so the kernel returned `ENOSYS`. Several
LTP cases use `tst_clock_settime()` in cleanup to restore wall clock state. That
made otherwise-progressing cases exit through `TBROK`/return code 34.

Before this change:

```text
clock_settime01.c:66: TFAIL: clock_settime(2) failed for clock CLOCK_REALTIME: ENOSYS (38)
tst_wallclock.c:64: TBROK: tst_clock_settime() realtime failed: ENOSYS (38)
FAIL LTP CASE clock_settime01 : 34

tst_wallclock.c:64: TBROK: tst_clock_settime() realtime failed: ENOSYS (38)
FAIL LTP CASE clock_adjtime01 : 34
FAIL LTP CASE clock_adjtime02 : 34
```

The fix is intentionally narrow:

- Add `clock_settime` dispatch.
- Support only `CLOCK_REALTIME`.
- Store a global realtime offset relative to the monotonic timer source.
- Apply that offset to `CLOCK_REALTIME`, `CLOCK_REALTIME_COARSE`, and
  `gettimeofday()`.
- Reject unsupported clocks or invalid `timespec` nanoseconds with `EINVAL`.

This does not attempt to implement real kernel time discipline, `clock_adjtime`
semantics, or per-namespace time state.

## Follow-up EFAULT Fix

After the realtime offset change, `clock_settime02` returned 0 as a whole case
but still contained one internal failure:

```text
clock_settime02.c:143: TFAIL: clock_settime(2): clock CLOCK_REALTIME passed unexpectedly, expected EFAULT: SUCCESS (0)
```

Root cause: LTP's `tst_get_bad_addr()` creates the bad pointer with:

```text
mmap(0, 1, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, 0, 0)
```

The kernel-side `read_user_value()` translated user addresses and then read the
physical memory directly, but did not require the translated mapping to be
user-accessible. `PROT_NONE` anonymous pages therefore looked readable to the
kernel helper even though user access should fault.

The follow-up fix keeps the change in syscall user-copy helpers, not in
`clock_settime()` itself:

- `read_user_value()`
- `read_user_cstr_bounded_len()`
- `read_user_cstr_array_bounded()`

Each translated byte must now come from a mapping with `MappingFlags::U`, or the
helper returns `EFAULT`.

## Fresh Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

### Formatting Check

Command:

```text
cargo fmt --manifest-path kernel/Cargo.toml --all -- --check
```

Result:

```text
exit code 0
```

### Clean Build

Command:

```text
rm -rf kernel/target kernel-rv kernel-la kernel/mount.img
make -B all
file kernel-rv kernel-la
readelf -h kernel-rv | grep -i flags
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

These were run with basic-only nested RV/LA images, so the harness ended
immediately after the two required basic groups.

Logs:

```text
.codex-logs/basic-only-rv-clock-settime-20260616.log
.codex-logs/basic-only-la-clock-settime-20260616.log
.codex-logs/basic-only-rv-clock-efault-20260616.log
.codex-logs/basic-only-la-clock-efault-20260616.log
```

Results:

```text
RISC-V:
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

LoongArch:
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

### Full LA cap350 Comparison

Previous log:

```text
/tmp/la-cap350-after-anon-mmap-fd.log
```

Previous result:

```text
RUN=350 returned0=105 TPASS=639 TFAIL=231 TBROK=54
initproc ltp: attempted 350 of 2840 cases, 105 returned 0
```

Current log:

```text
.codex-logs/la-cap350-clock-settime-20260616.log
.codex-logs/la-cap350-clock-efault-20260616.log
```

Current result:

```text
RUN=350 returned0=109 TPASS=654 TFAIL=231 TBROK=49
initproc ltp: attempted 350 of 2840 cases, 109 returned 0

After EFAULT follow-up:
RUN=350 returned0=109 TPASS=657 TFAIL=228 TBROK=49
initproc ltp: attempted 350 of 2840 cases, 109 returned 0
```

### Targeted Case Evidence

`clock_adjtime01` and `clock_adjtime02` still have internal expectation
failures, but no longer cleanup-break on `clock_settime()`:

```text
FAIL LTP CASE clock_adjtime01 : 0
FAIL LTP CASE clock_adjtime02 : 0
```

`clock_settime01` now exercises realtime advance and recede paths:

```text
clock_settime01.c:76: TPASS: clock_settime(2): was able to advance time
clock_settime01.c:97: TPASS: clock_settime(2): was able to recede time
FAIL LTP CASE clock_settime01 : 0
```

`clock_settime02` now validates invalid inputs, including the `PROT_NONE`
bad-address case:

```text
clock_settime02.c:150: TPASS: clock_settime(CLOCK_REALTIME): failed as expected: EFAULT (14)
clock_settime02.c:150: TPASS: clock_settime(CLOCK_REALTIME): failed as expected: EINVAL (22)
clock_settime02.c:150: TPASS: clock_settime(CLOCK_MONOTONIC): failed as expected: EINVAL (22)
clock_settime02.c:150: TPASS: clock_settime(INVALID/UNKNOWN CLOCK): failed as expected: EINVAL (22)
FAIL LTP CASE clock_settime02 : 0
```

## Remaining Known Gaps

- `clock_settime03` still returns 32/TCONF because `timer_create` syscall 107 is
  not implemented.
- This follow-up does not change whole-case returned-0 count because
  `clock_settime02` already returned 0 after the realtime offset fix. It improves
  internal LTP quality only: `TPASS +3`, `TFAIL -3` in the same cap350 window.
- Other syscalls still bypass the checked user-copy helper in places; for
  example `capget02` and `capset02` still show bad-address successes in the
  same cap350 run. That should be handled as a separate audit rather than mixed
  into the `clock_settime` patch.
