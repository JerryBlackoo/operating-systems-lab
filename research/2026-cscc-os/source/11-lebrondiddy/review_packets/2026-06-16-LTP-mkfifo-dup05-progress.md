# LTP mkfifo / dup05 Progress

Date: 2026-06-16 CST.

## Scope

This packet covers the minimal FIFO node support needed by LTP `dup05`.

The honest scoring result from this follow-up is:

```text
LA ltp-glibc cap350: 93 returned 0 -> 94 returned 0
Delta: +1 returned-count case
```

## Root Cause Addressed

`dup05` creates a FIFO before opening and duplicating it:

```text
mkfifo("dupfile", 0777)
open("dupfile", O_RDWR, 0700)
dup(fd)
```

Before this change, `mkfifo()` reached `mknodat(S_IFIFO)` and failed with
`ENOSYS`, so the case broke before testing `dup()`.

The implementation now adds a distinct VFS `FileType::Fifo`, maps FIFO stat
mode and ext4 inode types to it, supports creating FIFO nodes in ext4/ramfs,
and wires `mknodat()` for FIFO nodes only. Non-FIFO `mknodat()` remains
unsupported.

## Fresh Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

### Targeted RED

Log:

```text
/tmp/la-dup05-red.log
```

Result before the fix:

```text
RUN LTP CASE 000_dup05
dup05.c:28: TBROK: mkfifo(dupfile, 0777) failed: ENOSYS (38)
FAIL LTP CASE 000_dup05 : 2
```

Note: the targeted image's case-limit file write failed, so the run continued
after `000_dup05`. The first-case RED evidence above is still valid for the
targeted failure.

### Targeted GREEN

Log:

```text
/tmp/la-dup05-green2.log
```

Result after the fix:

```text
RUN LTP CASE 000_dup05
dup05.c:22: TPASS: dup(3) returned fd 4
FAIL LTP CASE 000_dup05 : 0
```

The same targeted-image caveat applies: the run continued after the first case,
and it was stopped after capturing the targeted `dup05` evidence.

### Clean Build

Log:

```text
.codex-logs/clean-build-mkfifo-20260616.log
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
.codex-logs/rv-basic-mkfifo-20260616.log
.codex-logs/la-basic-mkfifo-20260616.log
```

Basic group evidence:

```text
RV basic-glibc: group basic-glibc finished in 2s (exited, code 0)
RV basic-musl:  group basic-musl finished in 1s (exited, code 0)
LA basic-glibc: group basic-glibc finished in 2s (exited, code 0)
LA basic-musl:  group basic-musl finished in 2s (exited, code 0)
```

Note: the harness profile continued into later groups after the basic groups.
Those later groups are not part of this basic gate; the four basic groups above
completed with code 0.

### Full LA cap350 Comparison

Previous comparison log:

```text
/tmp/la-cap350-after-epolloneshot.log
```

Previous result:

```text
RUN LTP CASE dup05
dup05.c:28: TBROK: mkfifo(dupfile, 0777) failed: ENOSYS (38)
FAIL LTP CASE dup05 : 2
initproc ltp: attempted 350 of 2840 cases, 93 returned 0
```

Current log:

```text
/tmp/la-cap350-after-mkfifo.log
```

Current result:

```text
RUN LTP CASE dup05
dup05.c:22: TPASS: dup(3) returned fd 4
FAIL LTP CASE dup05 : 0
initproc ltp: attempted 350 of 2840 cases, 94 returned 0
```

Summary counters:

```text
previous cap350: RUN=350, returned0=93
current  cap350: RUN=350, TPASS=621, TFAIL=155, TBROK=70, returned0=94
```

## Remaining Known Gaps

- FIFO support is intentionally minimal: it creates a node with FIFO file type
  and stat mode, but does not implement Linux FIFO blocking/open semantics.
- The targeted image-limit write failed for the short RED/GREEN runs, so those
  logs are used only for first-case symptom evidence.
- `dirtypipe` still fails at `splice failed`.
- `epoll_wait03` still has an invalid-user-events internal `TFAIL`.
- `epoll_pwait03` still watchdogs in the full cap350 profile.
