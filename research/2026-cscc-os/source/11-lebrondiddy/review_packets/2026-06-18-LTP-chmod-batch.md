# 2026-06-18 LTP chmod path metadata batch

## Scope

Targeted the next `la-musl` chmod-family slice after the fchmod/fchmodat
metadata batch:

- `chmod03`
- `chmod06`
- `chmod07`

This packet is for the focused chmod gain only. It is not the final M2
judge-budget proof.

## Root Cause

The remaining chmod failures came from two syscall-level metadata gaps:

- `mkdirat()` created directories through the generic open/create path without
  applying the requested mode or current credentials to the new directory. That
  left directories with root/default metadata, so owner chmod checks in
  `chmod03` failed after the test switched users.
- `chmod()`/`fchmodat()` checked final file ownership and read-only mount state,
  but did not perform Linux-like path-prefix permission checks before following
  the final path. This caused the `chmod06` errno matrix to miss `EACCES`,
  `ENOTDIR`, and `ELOOP` cases.

`chmod07` was already helped by the sticky-bit and mode-mask work from the
previous fchmod batch; it stays in this focused slice because it validates the
chmod side of the same permission-bit behavior.

## Change

Touched:

- `kernel/kernel/src/syscall/fd.rs`
- `kernel/kernel/src/syscall/sys.rs`

Behavior changes:

- Reworked `sys_mkdir_at()` to:
  - validate the parent as a directory;
  - return `EEXIST` if the target already exists;
  - create the directory explicitly;
  - apply `mode & 0o7777`;
  - set owner/group to the current effective credentials.
- Updated `sys_chmod()` to:
  - reject an empty path with `ENOENT`;
  - check path-prefix permissions before final lookup;
  - follow the final symlink with `File::open_link()`;
  - reuse the common chmod helper for owner/root, read-only mount, and mode
    masking semantics.
- Updated `sys_fchmodat()` to:
  - reject an empty path with `ENOENT`;
  - validate relative `dirfd` directories;
  - check path-prefix permissions;
  - follow the final symlink unless `AT_SYMLINK_NOFOLLOW` is set;
  - reuse the common chmod helper.

The earlier `sys_ftruncate()` and fchmod/fchmodat metadata fixes remain in the
same working tree but are covered by their own review packets:

- `review_packets/2026-06-17-LTP-ftruncate03-batch.md`
- `review_packets/2026-06-18-LTP-fchmod-batch.md`

## Evidence

Format:

```text
.codex-logs/cargo-fmt-chmod-next-20260618.log
cargo fmt --all --check exited 0
Only warning observed: deprecated /root/.cargo/config message from the image.
```

Clean build / ABI:

```text
.codex-logs/force-make-chmod-next-20260618.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
```

Targeted RED baseline from the prior adjacent slice:

```text
.codex-logs/la-musl-chmod-fchmod-regress2-20260617.log
chmod03: failed before mkdirat metadata was fixed
chmod06: failed older path-permission errno cases
chmod07: pass after the sticky-bit/mode-mask work
initproc ltp: attempted 13 of 2833 cases, 9 returned 0
```

Targeted GREEN:

```text
.codex-logs/ltp-chmod-next-green2-20260618.log
chmod03 TPASS for file and directory 01777 mode
chmod06 TPASS for EPERM, EACCES, EFAULT, ENAMETOOLONG, ENOENT, ENOTDIR, EROFS, ELOOP
chmod07 TPASS for chmod(testfile, 01777)
initproc ltp: attempted 3 of 2823 cases, 3 returned 0
```

Adjacent chmod/fchmod regression slice:

```text
.codex-logs/ltp-chmod-fchmod-regress-20260618.log
chmod01: pass
chmod03: pass
chmod05: still TBROK due Group ID lookup ENETUNREACH
chmod06: pass
chmod07: pass
fchmod01/02/03/04/06: pass
fchmod05: still TBROK due Group ID lookup ENETUNREACH
fchmodat01/02: pass
initproc ltp: attempted 13 of 2833 cases, 11 returned 0
```

Fresh basic smoke evidence from full 4G images pruned to basic-only:

```text
.codex-logs/basic-only-full-la-chmod-next-e2fsck-20260618.log
.codex-logs/basic-only-full-rv-chmod-next-e2fsck-20260618.log
e2fsck repaired the temporary full-image copies before debugfs pruning.

.codex-logs/basic-la-full-basic-only-chmod-next-20260618.log
basic-glibc finished, code 0
basic-musl finished in 5s, code 0
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic-rv-full-basic-only-chmod-next-20260618.log
basic-glibc finished, code 0
basic-musl finished in 4s, code 0
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
```

Important limitation: earlier `basic-la-chmod-next-flat-mismatch-20260618.log`
and `basic-rv-chmod-next-flat-mismatch-20260618.log` used the 96M flat basic
images in the repository with the nested-image harness mode. Those logs are
validation setup errors, not regressions, and are not counted as evidence for
this packet.

## Result

Focused chmod slice:

```text
chmod03/chmod06/chmod07: 3 returned0
```

Adjacent chmod/fchmod family slice:

```text
13 attempted, 11 returned0
```

The two remaining non-zero cases in that family, `chmod05` and `fchmod05`, are
still the pre-existing group lookup `ENETUNREACH` TBROK cases.

No panic/page-fault string was observed in the focused or adjacent logs above.

## Remaining Work

- Continue the M2 long-tail grind beyond this focused batch.
- Run the four-quadrant judge-budget LTP pass with `TIMEOUT_SECONDS=6600` and
  `STALL_TIMEOUT_SECONDS=600`.
- Prove aggregate returned0 is greater than 841 on the current working tree.
- Only count real returned0 lines; do not count TBROK or harness noise.
