# 2026-06-17 LTP fchdir batch

## Scope

Targeted a narrow `la-musl` LTP slice:

- `fchdir01`
- `fchdir02`
- `fchdir03`

Root cause for the fixed subset:

- `Sysno::fchdir` was not dispatched, so all `fchdir()` calls returned `ENOSYS`.
- The existing `chdir()` path already had current-directory mutation logic; the missing piece was fd-based directory validation.

## Change

- Added `sys_fchdir(fd)`:
  - validates fd existence with `EBADF`;
  - requires the fd target to be a directory, otherwise `ENOTDIR`;
  - updates the process current directory from the fd target.
- Routed `Sysno::fchdir` in the syscall dispatcher.

Out of scope:

- Directory search permission checks for non-root credentials. `fchdir03` still expects `EACCES` after `seteuid(nobody)` on a directory with mode `0400`.

## Evidence

RED:

```text
.codex-logs/la-musl-fchdir01-03-red-20260617.log
initproc ltp: attempted 3 of 2823 cases, 0 returned 0
000_fchdir01 : 1
001_fchdir02 : 1
002_fchdir03 : 1
```

GREEN:

```text
.codex-logs/la-musl-fchdir01-03-green-20260617.log
initproc ltp: attempted 3 of 2823 cases, 2 returned 0
000_fchdir01 : 0
001_fchdir02 : 0
002_fchdir03 : 1
```

Delta:

```text
3 targeted cases: 0 -> 2 returned 0 (+2)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-fchdir-remote-20260617.log
.codex-logs/force-make-fchdir-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-fchdir-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-fchdir-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- `fchdir03`: needs real directory search permission checks after uid/gid changes.
