# 2026-06-17 LTP fcntl07 batch

## Scope

Targeted the remaining close-on-exec `fcntl` cases on `la-musl`:

- `fcntl07`
- `fcntl07_64`

## Root Cause

The focused RED initially showed every child returning 139 while checking
`FD_CLOEXEC` across a regular file, both pipe ends, and a FIFO.

Root causes:

- `exec_with_process()` cleared the current address space before it knew the
  requested executable and dynamic loader could be opened. In `execlp()`
  fallback paths this turned a normal `ENOENT` into a broken child process.
- Dynamic interpreter lookup used the caller cwd only. After `execlp()` found
  `/musl/ltp/testcases/bin/fcntl07` from `PATH`, the loader lookup still ran
  relative to the temporary cwd instead of the executable's `/musl/...` path.
- Successful exec did not close file descriptors marked `FD_CLOEXEC`.

## Change

- Keep the old process image intact until the new executable, ELF metadata, and
  dynamic interpreter are resolved successfully.
- Resolve the interpreter from the executed program's directory, then pass the
  resolved program path to the musl loader.
- Close fd-table entries whose descriptor copy carries `OpenFlags::CLOEXEC`
  at the successful exec switch point.

## Evidence

RED:

```text
.codex-logs/la-musl-fcntl07-red-20260617.log
fcntl07.c:135: regular file child exited non-zero, 139
fcntl07.c:135: pipe (write end) child exited non-zero, 139
fcntl07.c:135: pipe (read end) child exited non-zero, 139
fcntl07.c:135: fifo child exited non-zero, 139
initproc ltp: attempted 2 of 2 cases, 0 returned 0
```

Diagnostic RED:

```text
.codex-logs/la-musl-fcntl07-kernel-diag-red-20260617.log
fcntl07-trace: task=3 execve filename=/musl/fcntl07 args=["fcntl07", "-T", "3"]
```

This showed `execlp()` probing a missing `/musl/fcntl07`; before this batch,
that failed exec attempt had already destroyed the child's previous address
space.

GREEN:

```text
.codex-logs/la-musl-fcntl07-green-20260617.log
fcntl07: regular file CLOEXEC fd was closed after exec()
fcntl07: pipe (write end) CLOEXEC fd was closed after exec()
fcntl07: pipe (read end) CLOEXEC fd was closed after exec()
fcntl07: fifo CLOEXEC fd was closed after exec()
initproc ltp: attempted 2 of 2 cases, 2 returned 0
```

Regression slice:

```text
.codex-logs/la-musl-fcntl01-02-07-regress-20260617.log
initproc ltp: attempted 6 of 6 cases, 6 returned 0
```

Delta:

```text
2 targeted cases: 0 -> 2 returned 0 (+2)
fcntl01/fcntl02/fcntl07 regression slice: 6 -> 6 returned 0
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-fcntl07-20260617.log
.codex-logs/force-make-fcntl07-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic gates:

```text
.codex-logs/basic-rv-basic-only-fcntl07-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic-la-basic-only-fcntl07-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- Higher `fcntl` record-lock and lease cases remain out of scope.
