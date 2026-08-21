# 2026-06-17 LTP chown metadata batch

## Scope

Targeted a narrow `la-musl` ownership/mode slice:

- `chown05`
- `fchown05`
- `lchown01`
- `fchownat02`
- `chown03`
- `fchown03`

Root causes:

- RamFS did not persist uid/gid/mode metadata, so `chown()`/`fchown()`/`lchown()`
  calls could succeed while later `stat()` / `lstat()` still reported default
  ownership and mode.
- `fchmodat` was dispatched as if the old syscall had a fourth `flags`
  argument. LoongArch musl `chmod()` reaches old `fchmodat(dirfd, path, mode)`,
  so the kernel read a stale fourth register and returned `EINVAL`.
- `fstat`, `fstatat`, and `statx` forced `StatMode::OWNER_MASK` into every
  result. That hid real chmod mode bits, including `06770` in `chown03` and
  `fchown03`.

## Change

- Added VFS/File `chmod()` and `chown()` hooks.
- Added RamFS metadata storage for regular files, directories, hard links, and
  symlinks:
  - stored permission/setuid/setgid mode bits;
  - stored uid/gid;
  - honored `uid == gid == (u32)-1` no-change values;
  - cleared setuid/setgid when ownership changes.
- Applied create-time mode and owner metadata for `openat(..., O_CREAT, mode)`.
- Routed `fchmod`, old `fchmodat`, and `fchmodat2` separately:
  - old `fchmodat` passes `flags = 0`;
  - `fchmodat2` keeps the fourth `flags` argument.
- Removed the stat-layer `OWNER_MASK` override so filesystem metadata is the
  source of truth.

Out of scope:

- Full POSIX permission enforcement on all path operations.
- Ext4-backed persistent chmod/chown metadata. The target slice uses RamFS temp
  paths.

## Evidence

RED:

```text
.codex-logs/la-musl-chown-meta-red-20260617.log
chown05/fchown05/lchown01/fchownat02/chown03/fchown03:
ownership or mode verification failed after successful chown-family calls.
initproc ltp: attempted 6 of 2826 cases, 0 returned 0
```

Regression caught during implementation:

```text
.codex-logs/la-musl-chown-meta-green1-20260617.log
chmod(...,0666) failed: EINVAL
```

GREEN target slice:

```text
.codex-logs/la-musl-chown-meta-green3-20260617.log
FAIL LTP CASE 000_chown05 : 0
FAIL LTP CASE 001_fchown05 : 0
FAIL LTP CASE 002_lchown01 : 0
FAIL LTP CASE 003_fchownat02 : 0
FAIL LTP CASE 004_chown03 : 0
FAIL LTP CASE 005_fchown03 : 0
```

The focused image linked the six targets as `000_*` through `005_*`. The
`.ltp_case_limit` file was not honored in this run, so the log was manually
stopped after the target slice and later unrelated cases are not counted as
evidence for this packet.

Delta:

```text
6 targeted cases: 0 -> 6 returned 0 (+6)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-chown-meta-20260617.log
.codex-logs/force-make-chown-meta-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic gate:

```text
.codex-logs/basic-rv-basic-only-chown-meta-20260617.log
PASS: RISC-V basic markers and real basic case output found

.codex-logs/basic-la-basic-only-chown-meta-20260617.log
PASS: LoongArch basic markers and real basic case output found
```

## Remaining Work

- `lchown02` errno/permission details should be handled separately with focused
  RED evidence.
- `lchown03*` remains device/blocker-flavored and is not covered by this RamFS
  metadata batch.
- Ext4 metadata remains a broader filesystem model task if future tests require
  it.
