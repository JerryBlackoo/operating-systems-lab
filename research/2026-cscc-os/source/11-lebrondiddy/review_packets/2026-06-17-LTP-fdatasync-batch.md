# 2026-06-17 LTP batch: fdatasync syscall dispatch

## Scope

Cheap LTP syscall batch from the `la-musl` full baseline.

Fixed cases:

- `fdatasync01`
- `fdatasync02`

Out of scope:

- `fdatasync03`, still broken by missing/free block device setup:
  `TBROK: Failed to acquire device`.

## RED

Baseline:

```text
RUN LTP CASE fdatasync01
fdatasync01.c:100: fdatasync() failed, errno=38 : Function not implemented
FAIL LTP CASE fdatasync01 : 5

RUN LTP CASE fdatasync02
fdatasync02.c:135: Unexpected results for invalid file descriptor ; returned -1 (expected -1), errno 38 (expected 9)
fdatasync02.c:135: Unexpected results for file descriptor to a special file ; returned -1 (expected -1), errno 38 (expected 22)
FAIL LTP CASE fdatasync02 : 1
```

Targeted RED:

- `.codex-logs/la-musl-fdatasync01-03-red-20260617.log`
- `000_fdatasync01 : 1`
- `001_fdatasync02 : 1`
- `002_fdatasync03 : 2`
- `attempted 3 ... 0 returned 0`

## Implementation

Changed files:

- `kernel/kernel/src/syscall/fd.rs`
- `kernel/kernel/src/syscall/mod.rs`

Changes:

- Added `sys_fsync(fd)` helper.
- Routed both `fsync` and `fdatasync` through it.
- Validates fd existence with `EBADF`.
- Returns `EINVAL` for non-regular/special files.
- Returns success for regular files as a no-op flush, matching the kernel's prior `fsync` no-op policy.

## GREEN

Build and ABI:

- `.codex-logs/clean-build-fdatasync-20260617.log`
- `.codex-logs/incremental-make-fdatasync2-20260617.log`
- `kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI`
- `Flags: 0x1, RVC, soft-float ABI`

Formatting:

- `.codex-logs/fmt-check-fdatasync-20260617.log`

Targeted GREEN:

- `.codex-logs/la-musl-fdatasync01-03-green2-20260617.log`

```text
RUN LTP CASE 000_fdatasync01
fdatasync01    1  TPASS  :  fdatasync() successful
FAIL LTP CASE 000_fdatasync01 : 0

RUN LTP CASE 001_fdatasync02
fdatasync02    1  TPASS  :  Expected failure for invalid file descriptor, errno: 9
fdatasync02    2  TPASS  :  Expected failure for file descriptor to a special file, errno: 22
FAIL LTP CASE 001_fdatasync02 : 0

RUN LTP CASE 002_fdatasync03
TBROK: Failed to acquire device
FAIL LTP CASE 002_fdatasync03 : 2

initproc ltp: attempted 3 of 2823 cases, 2 returned 0
```

Basic-4 gate:

- `.codex-logs/basic4-rv-basic-only-fdatasync2-20260617.log`
  - `PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found`
- `.codex-logs/basic4-la-basic-only-fdatasync2-20260617.log`
  - `PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found`

## Delta

Targeted fdatasync mini-bucket:

```text
before: attempted 3, returned0 0
after:  attempted 3, returned0 2
delta:  +2 returned0
```
