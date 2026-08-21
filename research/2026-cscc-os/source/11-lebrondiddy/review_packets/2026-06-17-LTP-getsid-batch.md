# 2026-06-17 LTP getsid batch

## Scope

Targeted a narrow `la-musl` LTP slice:

- `getsid01`
- `getsid02`

Root cause:

- `Sysno::getsid` was not dispatched, so both the current-session query and the unused-pid error path returned `ENOSYS`.
- The current process model has no explicit session id field. Existing `setsid()` semantics detach the task from its parent, so the minimal compatible session model is to walk the parent chain to the detached leader and return that process id.

## Change

- Added `sys_getsid(pid)`:
  - `pid == 0` queries the current task;
  - nonzero `pid` must resolve through `tid2task`, otherwise returns `ESRCH`;
  - the session id is the process id of the top task in the parent chain.
- Routed `Sysno::getsid` in the syscall dispatcher.

Out of scope:

- A full POSIX session/process-group model with explicit session ids and controlling terminals.
- Permission checks between unrelated sessions.

## Evidence

RED:

```text
.codex-logs/la-musl-getsid01-02-red-20260617.log
initproc ltp: attempted 2 of 2822 cases, 0 returned 0
000_getsid01 : 1
001_getsid02 : 1
```

Key RED failures:

```text
getsid01.c:26: getsid(0) failed in parent: ENOSYS (38)
getsid02.c:22: getsid(unused_pid) expected ESRCH: ENOSYS (38)
```

GREEN:

```text
.codex-logs/la-musl-getsid01-02-green-20260617.log
getsid01.c:41: TPASS: p_sid == c_sid (2)
getsid02.c:22: TPASS: getsid(unused_pid) : ESRCH (3)
initproc ltp: attempted 2 of 2822 cases, 2 returned 0
000_getsid01 : 0
001_getsid02 : 0
```

Delta:

```text
2 targeted cases: 0 -> 2 returned 0 (+2)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-getsid-20260617.log
.codex-logs/force-make-getsid-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-getsid-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-getsid-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- Add an explicit session/process-group model only if later LTP cases require it.
- `setsid()` still returns a stub success value and only detaches from the parent chain.
