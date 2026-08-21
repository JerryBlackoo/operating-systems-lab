# 2026-06-17 LTP getpriority batch

## Scope

Targeted a narrow `la-musl` LTP slice:

- `getpriority01`
- `getpriority02`

Root cause:

- `Sysno::getpriority` was not dispatched, so both valid and invalid calls returned `ENOSYS`.
- Linux raw `getpriority(2)` returns `20 - nice`; libc converts that back to the user-visible nice value. For the current default nice value, the kernel syscall must return `20`, not `0`.

## Change

- Added `sys_getpriority(which, who)`:
  - accepts `PRIO_PROCESS`, `PRIO_PGRP`, and `PRIO_USER`;
  - invalid `which` returns `EINVAL`;
  - negative `who` returns `ESRCH`;
  - current process/group/user queries return raw syscall value `20`.
- Routed `Sysno::getpriority` in the syscall dispatcher.

Out of scope:

- Persistent nice values and `setpriority`.
- Full process-group/user enumeration beyond the current process test surface.

## Evidence

RED:

```text
.codex-logs/la-musl-getpriority01-02-red-20260617.log
initproc ltp: attempted 2 of 2822 cases, 0 returned 0
000_getpriority01 : 1
001_getpriority02 : 1
```

Key RED failures:

```text
getpriority01.c:37: getpriority(0, 0) failed: ENOSYS (38)
getpriority01.c:37: getpriority(1, 0) failed: ENOSYS (38)
getpriority01.c:37: getpriority(2, 0) failed: ENOSYS (38)
getpriority02.c:51: getpriority(-1, 0) should fail with EINVAL: ENOSYS (38)
getpriority02.c:51: getpriority(0, -1) should fail with ESRCH: ENOSYS (38)
```

GREEN:

```text
.codex-logs/la-musl-getpriority01-02-green-20260617.log
initproc ltp: attempted 2 of 2822 cases, 2 returned 0
000_getpriority01 : 0
001_getpriority02 : 0
```

Delta:

```text
2 targeted cases: 0 -> 2 returned 0 (+2)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-getpriority-20260617.log
.codex-logs/force-make-getpriority-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-getpriority-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-getpriority-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- `setpriority` and non-default nice accounting are still unsupported.
- Broader process-group/user priority lookup should be handled only if later LTP cases require it.
