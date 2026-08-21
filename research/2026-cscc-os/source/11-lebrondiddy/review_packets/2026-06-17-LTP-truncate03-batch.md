# 2026-06-17 LTP truncate03 batch

## Scope

Targeted the path-based truncate errno matrix:

- `truncate03`
- `truncate03_64`

`ftruncate03` and `ftruncate03_64` were reproduced in the same RED probe, but
left out of this code batch because they exercise a wider fd/socket/space
matrix.

## Root Cause

`sys_truncate(path, len)` delegated too quickly to `File::open_link()` and
`file.truncate()`:

- an empty path normalized to `/` and returned `EISDIR` instead of `ENOENT`;
- a non-directory path prefix surfaced as `ENOENT` instead of `ENOTDIR`;
- write permission on the named regular file was not checked for non-root
  effective credentials;
- `RLIMIT_FSIZE` was not stored, so over-limit truncate lengths did not return
  `EFBIG`.

## Change

- Added truncate-specific write permission checks using effective uid/gid.
- Validated non-final path components before opening the target file.
- Rejected empty truncate paths with `ENOENT`.
- Tracked `RLIMIT_FSIZE` through `prlimit64()` and rejected over-limit truncate
  lengths with `EFBIG`.
- Kept `ftruncate03*` out of scope for a separate fd-based batch.

## Evidence

RED probe:

```text
.codex-logs/la-musl-truncate03-ftruncate03-red-20260617.log
FAIL LTP CASE 000_truncate03 : 1
FAIL LTP CASE 001_truncate03_64 : 1
FAIL LTP CASE 002_ftruncate03 : 1
FAIL LTP CASE 003_ftruncate03_64 : 1
initproc ltp: attempted 4 of 4 cases, 0 returned 0
```

Selected `truncate03*` failures before the fix:

```text
truncate03.c:120: TFAIL: truncate(tc->pathname, tc->length) succeeded
truncate03.c:120: TFAIL: truncate(tc->pathname, tc->length) expected ENOTDIR: ENOENT (2)
truncate03.c:120: TFAIL: truncate(tc->pathname, tc->length) expected ENOENT: EISDIR (21)
truncate03.c:120: TFAIL: truncate(tc->pathname, tc->length) succeeded
```

GREEN:

```text
.codex-logs/la-musl-truncate03-only-image-20260617.log
000_truncate03
001_truncate03_64

.codex-logs/la-musl-truncate03-only-green-20260617.log
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : EACCES (13)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : ENOTDIR (20)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : EFAULT (14)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : ENAMETOOLONG (36)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : ENOENT (2)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : EISDIR (21)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : EFBIG (27)
truncate03.c:120: TPASS: truncate(tc->pathname, tc->length) : ELOOP (40)
FAIL LTP CASE 000_truncate03 : 0
FAIL LTP CASE 001_truncate03_64 : 0
initproc ltp: attempted 2 of 2 cases, 2 returned 0
```

Regression slice:

```text
.codex-logs/la-musl-truncate03-regress-image-20260617.log
000_ftest01
001_ftest03
002_ftest05
003_ftest07
004_truncate02
005_truncate02_64
006_truncate03
007_truncate03_64
008_ftruncate01
009_ftruncate01_64

.codex-logs/la-musl-truncate03-regress-20260617.log
initproc ltp: attempted 10 of 10 cases, 10 returned 0
```

Delta:

```text
2 targeted cases: 0 -> 2 returned 0 (+2)
truncate regression slice: 10 -> 10 returned 0
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-truncate03-20260617.log
.codex-logs/force-make-truncate03-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic gates:

```text
.codex-logs/basic-rv-basic-only-truncate03-20260617.log
PASS: RISC-V basic markers and real basic case output found

.codex-logs/basic-la-basic-only-truncate03-20260617.log
PASS: LoongArch basic markers and real basic case output found
```

## Remaining Work

- `ftruncate03` / `ftruncate03_64`: fd-based permission/socket/space errno
  matrix remains separate.
