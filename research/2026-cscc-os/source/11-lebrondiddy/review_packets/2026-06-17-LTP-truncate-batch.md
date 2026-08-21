# 2026-06-17 LTP truncate batch

## Scope

Targeted a narrow truncate syscall cluster on `la-musl`:

- `ftest01`
- `ftest03`
- `ftest05`
- `ftest07`
- `truncate02`
- `truncate02_64`
- `truncate03`
- `truncate03_64`
- `ftruncate01`
- `ftruncate01_64`
- `ftruncate03`
- `ftruncate03_64`

Root cause for the high-confidence subset:

- `Sysno::truncate` was not dispatched, so path-based truncate returned `ENOSYS`.
- `sys_ftruncate` returned `EINVAL` for invalid fd instead of Linux `EBADF`.

## Change

- Added `sys_truncate(path, len)`:
  - copies the user path with the existing bounded C-string helper;
  - resolves relative paths against `AT_FDCWD`;
  - follows symlinks through `File::open_link`;
  - rejects non-regular file targets with `EISDIR`;
  - delegates size mutation to the existing filesystem `truncate` path.
- Routed `Sysno::truncate` in the syscall dispatcher.
- Changed invalid-fd `sys_ftruncate` from `EINVAL` to `EBADF`.

Out of scope:

- `truncate03/_64` still have permission/path errno mismatches.
- `ftruncate03/_64` still have broader permission and ENOSPC expectations.

## Evidence

RED:

```text
.codex-logs/la-musl-truncate-ftest-red-20260617.log
initproc ltp: attempted 12 of 2832 cases, 2 returned 0
000_ftest01 : 1
001_ftest03 : 1
002_ftest05 : 1
003_ftest07 : 1
004_truncate02 : 1
005_truncate02_64 : 1
006_truncate03 : 1
007_truncate03_64 : 1
008_ftruncate01 : 0
009_ftruncate01_64 : 0
010_ftruncate03 : 1
011_ftruncate03_64 : 1
```

GREEN:

```text
.codex-logs/la-musl-truncate-ftest-green-20260617.log
initproc ltp: attempted 12 of 2832 cases, 8 returned 0
000_ftest01 : 0
001_ftest03 : 0
002_ftest05 : 0
003_ftest07 : 0
004_truncate02 : 0
005_truncate02_64 : 0
006_truncate03 : 1
007_truncate03_64 : 1
008_ftruncate01 : 0
009_ftruncate01_64 : 0
010_ftruncate03 : 1
011_ftruncate03_64 : 1
```

Delta:

```text
12 targeted cases: 2 -> 8 returned 0 (+6)
```

Build and ABI:

```text
.codex-logs/cargo-fmt-truncate-20260617.log
.codex-logs/clean-build-truncate-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-truncate-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-truncate-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- `truncate03/_64`: fixed later in `review_packets/2026-06-17-LTP-truncate03-batch.md`.
- `ftruncate03/_64`: needs ftruncate permission checks and ENOSPC/device setup behavior.
