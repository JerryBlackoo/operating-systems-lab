# LTP close_range Progress

Date: 2026-06-16 CST.

## Scope

This packet covers a narrow `close_range(2)` implementation for the LA
`ltp-glibc` `close_range02` case.

The honest scoring result is a clean-quality improvement, not a raw returned
case-count gain:

```text
close_range02 was already returning process status 0 in the cap350 baseline.
It now reports 11 TPASS and 0 TFAIL/TBROK in the targeted LTP walker run.
```

## Root Cause Addressed

The syscall was present in the `syscalls` crate but not dispatched by the kernel,
so LTP saw `ENOSYS` for every `close_range()` operation. The current cap350
baseline showed:

```text
.codex-logs/la-cap350-adjtimex-faccessat-v2-20260616.log
RUN LTP CASE close_range02
close_range02.c:52: TFAIL: close_range(3, 100, 0) failed: ENOSYS (38)
close_range02.c:54: TFAIL: fcntl(3, F_GETFD) succeeded
close_range02.c:56: TFAIL: fcntl(100, F_GETFD) succeeded
close_range02.c:106: TFAIL: close_range(3, 3, 0) failed: ENOSYS (38)
close_range02.c:59: TFAIL: close_range(4, 3, 0) expected EINVAL: ENOSYS (38)
close_range02.c:62: TFAIL: close_range(3, ~0U, ~0U) expected EINVAL: ENOSYS (38)
close_range02.c:65: TFAIL: close_range(~0U, ~0U, 0) failed: ENOSYS (38)
close_range02.c:71: TFAIL: close_range(4, 4, CLOSE_RANGE_CLOEXEC): ENOSYS (38)
close_range02.c:106: TFAIL: close_range(4, 4, 0) failed: ENOSYS (38)
close_range02.c:88: TFAIL: close_range(5, 5, CLOSE_RANGE_UNSHARE): ENOSYS (38)
close_range02.c:102: TPASS: 5 is open passed
close_range02.c:106: TFAIL: close_range(5, 5, 0) failed: ENOSYS (38)
FAIL LTP CASE close_range02 : 0
```

The implementation is intentionally small:

- Add `Sysno::close_range` dispatch.
- Reject `first > last` and unknown flag bits with `EINVAL`.
- Support `CLOSE_RANGE_CLOEXEC` by setting the fd's `CLOEXEC` flag.
- Treat `CLOSE_RANGE_UNSHARE` as a close operation for this single-process fd
  table model.
- Treat ranges beyond the fixed fd table as successful no-ops.

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

Log:

```text
.codex-logs/clean-build-close-range-20260616.log
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
.codex-logs/basic-only-rv-close-range-20260616.log
.codex-logs/basic-only-la-close-range-20260616.log
```

Results:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

### Targeted LTP Walker

The target image was a disposable repaired copy of the current LA LTP cap350
image. The original harness was not changed. The image linked
`close_range02` as `000_close_range02` and set `.ltp_case_limit=1`, so the normal
initproc LTP walker executed one regular LTP binary and stopped.

Log:

```text
.codex-logs/la-target-close-range02-only-20260616.log
```

Result:

```text
RUN LTP CASE 000_close_range02
close_range02.c:52: TPASS: close_range(3, 100, 0) passed
close_range02.c:54: TPASS: fcntl(3, F_GETFD) : EBADF (9)
close_range02.c:56: TPASS: fcntl(100, F_GETFD) : EBADF (9)
close_range02.c:59: TPASS: close_range(4, 3, 0) : EINVAL (22)
close_range02.c:62: TPASS: close_range(3, ~0U, ~0U) : EINVAL (22)
close_range02.c:65: TPASS: close_range(~0U, ~0U, 0) passed
close_range02.c:71: TPASS: close_range(3, 3, CLOSE_RANGE_CLOEXEC): SUCCESS (0)
close_range02.c:79: TPASS: FD_CLOEXEC was set on 3
close_range02.c:88: TPASS: close_range(3, 3, CLOSE_RANGE_UNSHARE): SUCCESS (0)
close_range02.c:95: TPASS: fcntl(3, F_GETFD) : EBADF (9)
close_range02.c:102: TPASS: 3 is open passed
FAIL LTP CASE 000_close_range02 : 0
initproc ltp: attempted 1 of 2841 cases, 1 returned 0
```

Summary counters for this targeted run:

```text
RUN=1 returned0=1 TPASS=11 TFAIL=0 TBROK=0
```

### Full Cap350 Attempt

A post-change LA cap350 attempt was started with:

```text
.codex-logs/la-cap350-close-range-20260616.log
```

It is not counted as a full cap350 result. The run reached only 120 LTP cases and
then the harness stopped at the pre-existing `change_password.sh` no-output
stall:

```text
RUN LTP CASE change_password.sh
HARNESS STALL: no console output for 120s
```

Counters at that point:

```text
RUN=120 returned0=28 TPASS=84 TFAIL=76 TBROK=23
```

Because that run never reached `close_range02`, the accepted post-change
evidence for this packet is the targeted LTP walker plus the clean build and
basic-4 gate above.

## Remaining Known Gaps

- `close_range01` still fails before reaching close-range semantics because it
  cannot acquire a test device.
- This implementation does not model Linux's full `CLOSE_RANGE_UNSHARE`
  copy-on-write file-table behavior. It is sufficient for the current fd-table
  model and the observed `close_range02` expectations.
- No OJ submission was made.
- No QEMU or Docker process was left running on the remote host after the runs.
