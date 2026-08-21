# 2026-06-17 LTP fchdir03 batch

## Scope

Targeted the remaining `la-musl` fchdir permission case:

- `fchdir03`

Root cause:

- `sys_fchdir()` validated that the fd existed and referenced a directory, but
  it did not enforce directory search permission after `seteuid(nobody)`.
- `sys_chdir()` shared the same current-directory mutation path, so the fix
  centralizes the permission check before updating `curr_dir`.

## Change

- Added a shared current-directory setter for `chdir()` and `fchdir()`.
- Added directory search permission checks against the target directory mode:
  - root (`euid == 0`) is allowed;
  - owner uses owner execute bit;
  - matching effective gid uses group execute bit;
  - otherwise uses other execute bit.
- Return `EACCES` before changing cwd when the effective credentials lack
  search permission.

## Evidence

RED:

```text
.codex-logs/la-musl-fchdir03-red-current-20260617.log
fchdir03.c:33: TFAIL: fchdir() succeeded unexpectedly
FAIL LTP CASE 000_fchdir03 : 1
initproc ltp: attempted 1 of 2821 cases, 0 returned 0
```

GREEN:

```text
.codex-logs/la-musl-fchdir03-green-20260617.log
fchdir03.c:42: TPASS: fchdir() failed expectedly: EACCES (13)
FAIL LTP CASE 000_fchdir03 : 0
initproc ltp: attempted 1 of 2821 cases, 1 returned 0
```

Regression slice:

```text
.codex-logs/la-musl-fchdir03-regress-image-20260617.log
000_fchdir01
001_fchdir02
002_fchdir03

.codex-logs/la-musl-fchdir01-03-regress-fchdir03-20260617.log
FAIL LTP CASE 000_fchdir01 : 0
FAIL LTP CASE 001_fchdir02 : 0
FAIL LTP CASE 002_fchdir03 : 0
initproc ltp: attempted 3 of 3 cases, 3 returned 0
```

Delta:

```text
1 targeted case: 0 -> 1 returned 0 (+1)
fchdir01/fchdir02/fchdir03 regression slice: 3 -> 3 returned 0
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-fchdir03-20260617.log
.codex-logs/force-make-fchdir03-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic gates:

```text
.codex-logs/basic-rv-basic-only-fchdir03-20260617.log
PASS: RISC-V basic markers and real basic case output found

.codex-logs/basic-la-basic-only-fchdir03-20260617.log
PASS: LoongArch basic markers and real basic case output found
```

## Remaining Work

- No known remaining `fchdir01`/`fchdir02`/`fchdir03` issue in the focused
  current-tree slice.
