# LTP umount mntpoint/ovl EINVAL clear-noise check

Date: 2026-06-15 CST.

## Scope

This packet covers the small `sys_umount2()` cleanup that removes the hard-coded
`EINVAL` return for paths containing `mntpoint`.

The goal is not to claim new real LTP pass credit. The goal is to remove a known
garbage TBROK amplifier so later LTP work can measure real failures instead of
hundreds of thousands of repeated cleanup errors.

## Symptom

Historical full-run logs contained a huge number of repeated cleanup failures:

```text
umount('mntpoint/ovl') failed with EINVAL
umount(mntpoint/ovl) failed: EINVAL (22)
```

Counts from the historical full logs:

```text
/tmp/full-la.log.gz: 361698 matching mntpoint/ovl EINVAL lines
/tmp/full-rv.log.gz: 275293 matching mntpoint/ovl EINVAL lines
```

The early LTP windows already showed the problem:

```text
old LA early window: 122 matching mntpoint/ovl EINVAL lines
old RV early LTP window: 52 matching mntpoint/ovl EINVAL lines
```

## Root Cause

`kernel/kernel/src/syscall/fd.rs::sys_umount2()` returned `EINVAL` for any path
containing `mntpoint`:

```rust
if special.contains("mntpoint") {
    return Err(Errno::EINVAL);
}
```

That made LTP cleanup of `/glibc/mntpoint/ovl` fail even though the kernel mount
path currently treats shallow `tmpfs`/`overlay` mounts as successful synthetic
mounts.

The filesystem `umount(path)` path only removes matching mount-table entries and
returns success if no entry is present, so deleting the hard-coded branch is a
low-risk consistency fix.

## Fix

Deleted the `special.contains("mntpoint") -> EINVAL` branch from
`sys_umount2()`. Other behavior is unchanged:

- `/dev*` umount still returns `ENOSYS`.
- Other paths still flow into `umount(special.as_str())`.

## Fresh Verification

All commands below ran on the remote host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

### Clean Build

Log:

```text
.codex-logs/clean-build-umount-mntpoint-20260615.log
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
.codex-logs/rv-basic-only-umount-mntpoint-verified-20260615.log
.codex-logs/la-basic-only-umount-mntpoint-verified-20260615.log
```

Results:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

The gate used disposable judge-like basic-only images with only
`basic_testcode.sh` left under `/glibc` and `/musl`, matching the prior verified
basic gate shape.

### Targeted LTP Windows

Logs:

```text
.codex-logs/rv-ltp-glibc-umount-cap25-20260615.log
.codex-logs/la-ltp-glibc-umount-cap350-20260615.log
```

Current targeted results:

```text
RV ltp-glibc cap25:
  RUN LTP CASE lines: 25
  returned 0: 9
  mntpoint/ovl EINVAL lines: 0
  panic/page/stall signals: 0
  END marker and harness PASS present

LA ltp-glibc cap350:
  RUN LTP CASE lines: 350
  returned 0: 86
  mntpoint/ovl EINVAL lines: 0
  panic/page/stall signals: 0
  END marker and harness PASS present
```

Historical comparison at the same early failure area:

```text
old RV early LTP window: 52 matching mntpoint/ovl EINVAL lines
new RV cap25 window: 0 matching mntpoint/ovl EINVAL lines

old LA early window: 122 matching mntpoint/ovl EINVAL lines
new LA cap350 window: 0 matching mntpoint/ovl EINVAL lines
```

Example behavior shift:

- Old LA `access01` cleanup repeatedly printed
  `umount(mntpoint/ovl) failed: EINVAL (22)`.
- New LA `access01` reaches its real access/permission failures instead; there
  are no `mntpoint/ovl` cleanup TBROK lines in the run.

## Notes

This is a clear noise-reduction and measurement-quality fix. It should not be
counted as honest LTP pass growth by itself. The next P1 step still needs real
whole-case growth from actual syscall/filesystem/procfs behavior, not from
garbage TBROK removal.
