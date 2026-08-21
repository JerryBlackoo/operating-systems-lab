# 2026-06-17 LTP ftruncate03 batch

## Scope

Targeted the remaining `la-musl` ftruncate errno slice:

- `ftruncate03`
- `ftruncate03_64`

## Root Cause

`sys_ftruncate()` delegated straight to `File::truncate()` after fd lookup.
That skipped the fd-level Linux errno matrix that this LTP build checks:

- non-regular fds, such as sockets, should fail before filesystem truncate;
- read-only regular fds should fail before filesystem truncate;
- negative `off_t` values passed as `usize` should be rejected before truncate;
- ordinary bad fds should remain `EBADF`.

The earlier RED logs were easy to misread: in `ftruncate03.c` output, `got %s`
is the testcase table's expected errno string, while the trailing
`expected: ERRNO (n)` text reports the actual `TST_ERR`.

## Change

Touched only:

- `kernel/kernel/src/syscall/fd.rs`

`sys_ftruncate()` now:

- returns `EINVAL` for negative lengths;
- returns `EBADF` for missing fd entries;
- returns `EINVAL` for non-regular files;
- returns `EINVAL` for fds not opened writable;
- checks `RLIMIT_FSIZE` before delegating to `file.truncate()`.

All temporary `diag-ftruncate`, syscall-return probes, and initproc case
filters were removed before verification.

## Evidence

Format:

```text
.codex-logs/cargo-fmt-ftruncate03-final-20260617.log
cargo fmt --all --check: PASS
```

Clean build / ABI:

```text
.codex-logs/force-make-ftruncate03-final-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
```

Targeted GREEN:

```text
.codex-logs/la-musl-ftruncate03-green2-20260617.log
RUN LTP CASE 000_ftruncate03
FAIL LTP CASE 000_ftruncate03 : 0
RUN LTP CASE 001_ftruncate03_64
FAIL LTP CASE 001_ftruncate03_64 : 0
initproc ltp: attempted 2 of 2822 cases, 2 returned 0
bad=[]
```

Adjacent regression slice:

```text
.codex-logs/la-musl-truncate-ftruncate-regress-20260617.log
000_truncate03      : 0
001_truncate03_64   : 0
002_ftruncate01     : 0
003_ftruncate01_64  : 0
004_ftruncate03     : 0
005_ftruncate03_64  : 0
initproc ltp: attempted 6 of 2826 cases, 6 returned 0
bad=[]
```

Basic gate on full 4G sdcard images:

```text
.codex-logs/basic-la-full-ftruncate03-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic-rv-full-ftruncate03-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
```

## Result

Focused delta:

```text
ftruncate03*: 0 -> 2 returned0
```

No panic/page-fault was observed in the focused or adjacent regression slices.

## Next

This is a small positive returned0 batch, not the full M2 finish line. The
remaining objective still requires continuing the long-tail grind and, before
finalizing the branch, a fresh four-quadrant LTP judge-budget run proving
aggregate returned0 > 841 with no new panic/page fault.
