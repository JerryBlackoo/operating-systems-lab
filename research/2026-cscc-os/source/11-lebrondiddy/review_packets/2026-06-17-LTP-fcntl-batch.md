# 2026-06-17 LTP fcntl batch

## Scope

Targeted a narrow `fcntl` subset on `la-musl`:

- `fcntl01`
- `fcntl01_64`
- `fcntl02`
- `fcntl02_64`

Root cause:

- `F_DUPFD` and `F_DUPFD_CLOEXEC` ignored the caller-provided minimum fd and always allocated the first free fd.
- `F_SETFL` replaced the whole open-file flag set and dropped the access mode bits, so `F_GETFL` later missed `O_WRONLY`.

## Change

- Added `UserTask::alloc_fd_at_or_above(min_fd)`.
- Routed `F_DUPFD` and `F_DUPFD_CLOEXEC` through the new min-fd-aware allocator.
- Changed `F_SETFL` to preserve `OpenFlags::ACCMODE` and `OpenFlags::CLOEXEC` while replacing mutable status flags.

Out of scope:

- At the time of this `fcntl01/02` batch, `fcntl07/_64` still exited 139 in children after exec; it is covered by the later `fcntl07` batch.
- record locks, leases, and higher `fcntl` cases remain untouched.

## Evidence

RED:

```text
.codex-logs/la-musl-fcntl01-02-red-20260617.log
000_fcntl01    : 1
001_fcntl01_64 : 1
002_fcntl02    : 1
003_fcntl02_64 : 1
initproc ltp: attempted 4 of 2824 cases, 0 returned 0

fcntl01: F_DUPFD returned fd 7 where 8 was expected.
fcntl01: F_SETFL(0) left flags at 0x8000 instead of preserving O_WRONLY.
fcntl02: F_DUPFD with min fd 10/100 returned 4.
```

GREEN:

```text
.codex-logs/la-musl-fcntl01-02-green-20260617.log
000_fcntl01    : 0
001_fcntl01_64 : 0
002_fcntl02    : 0
003_fcntl02_64 : 0
initproc ltp: attempted 4 of 2824 cases, 4 returned 0

fcntl02: F_DUPFD with min fd 10 returned 10.
fcntl02: F_DUPFD with min fd 100 returned 100.
```

Build and ABI:

```text
.codex-logs/cargo-fmt-fcntl-20260617.log
.codex-logs/force-make-fcntl-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-fcntl-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-fcntl-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```
