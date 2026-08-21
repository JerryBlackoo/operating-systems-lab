# 2026-06-17 LTP lchown02 batch

## Scope

Targeted one `la-musl` errno/permission case:

- `lchown02`

Root cause:

- On RV/LA musl, `lchown()` reaches the kernel through
  `fchownat(AT_FDCWD, path, uid, gid, AT_SYMLINK_NOFOLLOW)`.
- The `fchownat`/`lchown` path updated RamFS metadata directly after lookup, so
  it missed several Linux error checks:
  - non-root non-owner changing symlink ownership should fail with `EPERM`;
  - missing search permission on an intermediate directory should fail with
    `EACCES`;
  - a regular file used as a path prefix should fail with `ENOTDIR`;
  - an empty path without `AT_EMPTY_PATH` should fail with `ENOENT`.

## Change

- Added chown-family path prefix validation for `fchownat` and `lchown`.
- Checked directory execute/search bits for the current effective uid/gid.
- Rejected non-root ownership changes unless the current effective uid owns the
  target inode.
- Preserved existing root success paths used by `lchown01`, `fchownat02`, and
  the prior chown metadata batch.

Out of scope:

- Full POSIX ownership/group-change semantics for every chown-family syscall.
- General VFS-wide permission enforcement on lookup/open.
- Device-acquisition cases such as `lchown03*`.

## Evidence

RED:

```text
.codex-logs/la-musl-lchown02-red-20260617.log
lchown02:
EPERM expected but lchown returned 0
EACCES expected but lchown returned 0
ENOTDIR expected but errno was ENOENT
ENOENT expected for empty path but lchown returned 0
initproc ltp: attempted 1 of 1 cases, 0 returned 0
```

GREEN target:

```text
.codex-logs/la-musl-lchown02-green-20260617.log
lchown02: EPERM/EACCES/EFAULT/ENAMETOOLONG/ENOTDIR/ENOENT subcases all TPASS
FAIL LTP CASE 000_lchown02 : 0
initproc ltp: attempted 1 of 1 cases, 1 returned 0
```

Regression slice:

```text
.codex-logs/la-musl-chown-meta-regress-lchown02-20260617.log
FAIL LTP CASE 000_chown05 : 0
FAIL LTP CASE 001_fchown05 : 0
FAIL LTP CASE 002_lchown01 : 0
FAIL LTP CASE 003_fchownat02 : 0
FAIL LTP CASE 004_chown03 : 0
FAIL LTP CASE 005_fchown03 : 0
initproc ltp: attempted 6 of 6 cases, 6 returned 0

.codex-logs/la-musl-fchownat01-regress-lchown02-20260617.log
FAIL LTP CASE 000_fchownat01 : 0
initproc ltp: attempted 1 of 1 cases, 1 returned 0
```

Delta:

```text
1 targeted case: 0 -> 1 returned 0 (+1)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-lchown02-20260617.log
.codex-logs/force-make-lchown02-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic gate:

```text
.codex-logs/basic-rv-basic-only-lchown02-20260617.log
PASS: RISC-V basic markers and real basic case output found

.codex-logs/basic-la-basic-only-lchown02-20260617.log
PASS: LoongArch basic markers and real basic case output found
```

## Remaining Work

- `lchown03*` remains a device/blocker-flavored family.
- Broader directory permission/search checks may help `fchdir03`, but that
  should be handled as a separate focused batch.
