# 2026-06-17 LTP ftruncate03 failed attempt

## Scope

Targeted the remaining medium-cost `la-musl` slice:

- `ftruncate03`
- `ftruncate03_64`

## Attempted Change

An uncommitted `sys_ftruncate()` patch tried to add fd-based errno checks:

- negative length returns `EINVAL`;
- non-regular fd returns `EINVAL`;
- non-writable fd returns `EINVAL`;
- `RLIMIT_FSIZE` overflow returns `EFBIG`;
- otherwise delegate to `file.truncate(len)`.

The patch touched only:

- `kernel/kernel/src/syscall/fd.rs`

## Evidence

Build / ABI evidence from the failed attempt:

```text
.codex-logs/force-make-ftruncate03-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
```

Focused `la-musl` runs remained red:

```text
.codex-logs/la-musl-ftruncate03-green1-20260617.log
.codex-logs/la-musl-ftruncate03-green2-20260617.log
.codex-logs/la-musl-ftruncate03-current-red-20260617.log
initproc ltp: attempted 2 of 2822 cases, 0 returned 0
```

The failures stayed in the same matrix:

```text
ftruncate03.c:60: TFAIL: ftruncate() failed unexpectedly, got EINVAL, expected: EPERM
ftruncate03.c:60: TFAIL: ftruncate() failed unexpectedly, got EINVAL, expected: EPERM
ftruncate03.c:60: TFAIL: ftruncate() failed unexpectedly, got EINVAL, expected: ENOSPC
ftruncate03.c:60: TFAIL: ftruncate() failed unexpectedly, got EBADF, expected: EPERM
```

## Result

Delta:

```text
2 targeted cases: 0 -> 0 returned 0
```

The patch was not committed. It was isolated in:

```text
stash@{Wed Jun 17 05:04:30 2026}: On wip/ltp-reach-20260616: failed-ftruncate03-wip-20260617
```

## Next Read

Do not replay this exact fd/type/limit check as another batch. The remaining
root cause likely needs test-source-level triage of `ftruncate03.c` case setup,
especially why the final subcase expects `EPERM` after the kernel currently
sees `EBADF`.
