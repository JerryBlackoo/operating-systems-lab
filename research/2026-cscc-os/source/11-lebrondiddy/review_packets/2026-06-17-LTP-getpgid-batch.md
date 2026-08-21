# 2026-06-17 LTP getpgid batch

## Scope

Targeted a narrow `la-musl` LTP slice:

- `getpgid01`
- `getpgid02`

Root cause:

- The dispatcher ignored the `getpgid(pid)` argument and routed every call to a
  no-argument stub returning process group `0`.
- This was enough for current-process and child-process checks, but it made
  `getpgid(1)` disagree with `/proc/1/stat`, whose pgid field is `1`.
- Invalid negative and unused positive pids also incorrectly succeeded instead
  of returning `ESRCH`.

## Change

- Routed `Sysno::getpgid` with `args[0]`.
- Added minimal pid handling in `sys_getpgid(pid)`:
  - negative pid values return `ESRCH`;
  - `pid == 0` preserves the current process-group model and returns `0`;
  - `pid == 1` returns `1` to match the static procfs init pgid;
  - other existing task ids return `0`;
  - unknown nonzero task ids return `ESRCH`.

Out of scope:

- A full POSIX process-group/session model.
- `setpgid()` semantics beyond the existing no-op success behavior.

## Evidence

RED:

```text
.codex-logs/la-musl-getpgid01-02-red-20260617.log
getpgid01.c:48: TFAIL: TST_RET (0) != get_init_pgid() (1)
getpgid02.c:27: TFAIL: getpgid(-99) succeeded
getpgid02.c:28: TFAIL: getpgid(4194304) succeeded
initproc ltp: attempted 2 of 2822 cases, 0 returned 0
000_getpgid01 : 1
001_getpgid02 : 1
```

GREEN:

```text
.codex-logs/la-musl-getpgid01-02-green-20260617.log
getpgid01.c:47: TPASS: getpgid(1) returned pid 1
getpgid01.c:48: TPASS: TST_RET == get_init_pgid() (1)
getpgid02.c:27: TPASS: getpgid(-99) : ESRCH (3)
getpgid02.c:28: TPASS: getpgid(4194304) : ESRCH (3)
initproc ltp: attempted 2 of 2822 cases, 2 returned 0
000_getpgid01 : 0
001_getpgid02 : 0
```

Delta:

```text
2 targeted cases: 0 -> 2 returned 0 (+2)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-getpgid-20260617.log
.codex-logs/force-make-getpgid-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-getpgid-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-getpgid-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- Add explicit process-group state only if later LTP process-control cases require
  real `setpgid()` or cross-session semantics.
