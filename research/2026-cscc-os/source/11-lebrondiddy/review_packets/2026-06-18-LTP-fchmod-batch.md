# 2026-06-18 LTP fchmod/chmod metadata batch

## Scope

Targeted the `la-musl` chmod-family slice that was still returning non-zero:

- `fchmod02`
- `fchmod03`
- `fchmod04`
- `fchmod06`
- `fchmodat02`

This packet is for the focused fchmod/fchmodat gain only. It is not the final
M2 judge-budget proof.

## Root Cause

The failures split into three related metadata semantics gaps:

- `StatMode` did not represent the sticky bit, so `01777` mode checks lost the
  high permission bit.
- `chmod`, `fchmod`, and `fchmodat` delegated directly to `File::chmod()`
  without Linux-style owner/root checks or mode masking.
- `fchmodat()` handled an empty pathname after relative `dirfd` validation,
  returning `ENOTDIR` where LTP expects `ENOENT`.
- The tmpfs/overlay mount fast path ignored `MS_RDONLY`, so chmod on a
  read-only tmpfs mount could succeed instead of returning `EROFS`.

## Change

Touched:

- `kernel/crates/libc-types/src/arch/mod.rs`
- `kernel/filesystem/fs/src/dentry.rs`
- `kernel/kernel/src/syscall/fd.rs`
- `kernel/kernel/src/syscall/sys.rs`

Behavior changes:

- Added `StatMode::STICKY = 0o1000`.
- Added a small read-only mount registry for the tmpfs/overlay fast path and
  cleanup on unmount.
- Recorded `MS_RDONLY` during `sys_mount()` for tmpfs/overlay mounts.
- Routed `chmod`, `fchmod`, and `fchmodat` through a common helper that:
  - rejects read-only mount paths with `EROFS`;
  - allows only root or file owner to change mode;
  - stores only `mode & 0o7777`.
- Returned `ENOENT` for empty `fchmodat()` path before `dirfd` type checks.

The prior `sys_ftruncate()` fix remains in the same working tree but is covered
by `review_packets/2026-06-17-LTP-ftruncate03-batch.md`.

## Evidence

Format:

```text
.codex-logs/cargo-fmt-fchmod-20260618.log
cargo fmt --all --check exited 0
Only warning observed: deprecated /root/.cargo/config message from the image.
```

Clean build / ABI:

```text
.codex-logs/force-make-fchmod-20260618.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
```

Targeted RED:

```text
.codex-logs/la-musl-fchmod-red3-20260617.log
fchmod02: incorrect mode 0100777, expected 01777
fchmod03: incorrect mode 0100777, expected 0777
fchmod04: incorrect mode 040777, expected 01777
fchmod06: fchmod passed unexpectedly for EPERM/EROFS cases
fchmodat02: empty path returned ENOTDIR instead of ENOENT
initproc ltp: attempted 5 of 2825 cases, 0 returned 0
```

Targeted GREEN:

```text
.codex-logs/la-musl-fchmod-green3-20260617.log
fchmod02 TPASS
fchmod03 TPASS
fchmod04 TPASS
fchmod06 TPASS for EPERM, EBADF, EROFS
fchmodat02 TPASS for ENOTDIR, EBADF, EFAULT, ENAMETOOLONG, ENOENT, EINVAL
initproc ltp: attempted 5 of 2825 cases, 5 returned 0
```

Adjacent chmod/fchmod slice:

```text
.codex-logs/la-musl-chmod-fchmod-regress2-20260617.log
chmod01: pass
chmod03: still fails on directory sticky chmod, pre-existing unresolved case
chmod05: still TBROK due Group ID lookup ENETUNREACH
chmod06: still has older path-permission errno failures
chmod07: pass
fchmod01/02/03/04/06: pass
fchmod05: still TBROK due Group ID lookup ENETUNREACH
fchmodat01/02: pass
initproc ltp: attempted 13 of 2833 cases, 9 returned 0
```

Basic smoke evidence from full 4G images:

```text
.codex-logs/basic-la-full-basic-only-fchmod-20260617.log
basic-glibc finished in 7s, code 0
basic-musl finished in 5s, code 0

.codex-logs/basic-rv-full-basic-only-fchmod-20260617.log
basic-glibc finished in 6s, code 0
basic-musl finished in 4s, code 0
```

Important limitation: the attempted debugfs pruning for these two "basic-only"
images failed with "Filesystem not open", so those logs prove the basic groups
started and finished before the later groups, but they are not a clean basic-4
gate. A fresh clean basic-4 gate is still required before finalizing the branch.

## Result

Focused delta:

```text
fchmod02/fchmod03/fchmod04/fchmod06/fchmodat02: 0 -> 5 returned0
```

No panic/page-fault string was observed in the focused or adjacent logs above.

## Remaining Work

- Continue the M2 long-tail grind beyond this focused batch.
- Run a fresh clean `basic-4` gate on the current working tree.
- Run the four-quadrant judge-budget LTP pass with `TIMEOUT_SECONDS=6600` and
  `STALL_TIMEOUT_SECONDS=600`.
- Only count real returned0 lines; do not count TBROK or harness noise.
