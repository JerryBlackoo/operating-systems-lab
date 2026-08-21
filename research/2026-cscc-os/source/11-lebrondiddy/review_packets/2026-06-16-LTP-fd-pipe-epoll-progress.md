# LTP fd/pipe/epoll Progress

Date: 2026-06-16 CST.

## Scope

This packet covers the current LTP fd/pipe/epoll batch after the earlier RV
page-table panic fix and LA `umount(mntpoint/ovl)` cleanup.

The honest scoring result from this batch is:

```text
LA ltp-glibc cap350: 92 returned 0 -> 93 returned 0
Delta: +1 real returned-count case, from epoll_wait06
```

`dirtypipe` is still failing at `splice failed` and is not counted. The
CLOEXEC/dup/epoll_create1 work removes internal LTP TFAILs in targeted cases,
but the preceding full cap350 run stayed at 92 returned 0, so it is not counted
as returned-count growth.

## Root Causes Addressed

### FD flags and shared offsets

LTP dup/dup3/epoll_create1 cases exposed two file descriptor semantics issues:

- duplicated file descriptors need to share file offset state;
- FD flags such as `FD_CLOEXEC` need to be per descriptor, not shared file
  state.

The implementation now shares `File::offset` through `Arc<Mutex<usize>>` while
leaving `flags` independently cloned per descriptor. `dup3`,
`F_DUPFD_CLOEXEC`, `F_GETFD`, `F_SETFD`, `F_SETFL`, and `epoll_create1` now
handle CLOEXEC and invalid flag cases more closely to Linux.

### Pipe size and edge-triggered epoll

`dirtypipe` and `epoll_wait06` both reached missing pipe capacity behavior:

- `F_GETPIPE_SZ` / `F_SETPIPE_SZ` were not recognized;
- `pipe2()` ignored `O_NONBLOCK` / `O_CLOEXEC`;
- epoll did not remember prior readiness for `EPOLLET`;
- pipe write-side readiness was too permissive for the edge-triggered pipe test.

The pipe implementation now has shared `PipeState { queue, capacity }` with a
default capacity of `0x50000` and a minimum capacity of `4096`. `fcntl()` can
query/set pipe capacity on both pipe ends. `pipe2()` validates and applies
`O_NONBLOCK | O_CLOEXEC`. `EpollFile` now tracks last readiness per interest,
which is enough for `epoll_wait06`'s EPOLLET sequence. Pipe `OUT` polling now
requires at least `4096` free bytes.

## Fresh Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

### Clean Build

Log:

```text
.codex-logs/clean-build-fd-pipe-epoll-20260616.log
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
.codex-logs/rv-basic-fd-pipe-epoll-20260616.log
.codex-logs/la-basic-fd-pipe-epoll-20260616.log
```

Basic group evidence:

```text
RV basic-glibc: group basic-glibc finished in 2s (exited, code 0)
RV basic-musl:  group basic-musl finished in 1s (exited, code 0)
LA basic-glibc: group basic-glibc finished in 2s (exited, code 0)
LA basic-musl:  group basic-musl finished in 2s (exited, code 0)
```

Note: the local harness/image profile continued into later groups after the
basic groups. Those later groups are not part of this basic gate; the four basic
groups above completed with code 0.

### Targeted Pipe/Epoll Window

Log:

```text
/tmp/la-pipesz-target-after-pipeout.log
```

Result:

```text
dirtypipe.c:122: TFAIL: splice failed
epoll_wait06.c:80: TPASS: write() failed : EAGAIN/EWOULDBLOCK (11)
epoll_wait06.c:82: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 1 (1)
epoll_wait06.c:91: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 0 (0)
epoll_wait06.c:98: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 1 (1)
epoll_wait06.c:100: TPASS: evt_receive.events & EPOLLOUT == EPOLLOUT (4)
initproc ltp: attempted 5 of 2842 cases, 2 returned 0
```

This targeted run confirms `epoll_wait06` internally passes. It also confirms
`dirtypipe` still fails later at `splice failed`; this batch does not claim that
case.

### Full LA cap350 Delta

Previous comparison log:

```text
/tmp/la-cap350-after-cloexec.log
```

Previous result after CLOEXEC/dup/epoll_create1:

```text
RUN LTP CASE epoll_wait06
epoll_wait06.c:43: TBROK: fcntl(4,F_SETPIPE_SZ,...) failed: EINVAL (22)
initproc ltp: attempted 350 of 2840 cases, 92 returned 0
```

Current log:

```text
/tmp/la-cap350-after-pipesz-epollet.log
```

Current result:

```text
RUN LTP CASE epoll_wait06
epoll_wait06.c:80: TPASS: write() failed : EAGAIN/EWOULDBLOCK (11)
epoll_wait06.c:82: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 1 (1)
epoll_wait06.c:83: TPASS: evt_receive.data.fd == fds[0] (3)
epoll_wait06.c:84: TPASS: evt_receive.events & EPOLLIN == EPOLLIN (1)
epoll_wait06.c:91: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 0 (0)
epoll_wait06.c:96: TPASS: read() failed : EAGAIN/EWOULDBLOCK (11)
epoll_wait06.c:98: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 1 (1)
epoll_wait06.c:99: TPASS: evt_receive.data.fd == fds[1] (4)
epoll_wait06.c:100: TPASS: evt_receive.events & EPOLLOUT == EPOLLOUT (4)
initproc ltp: attempted 350 of 2840 cases, 93 returned 0
```

The full cap350 run executed exactly 350 LTP cases and `epoll_wait06` no longer
has internal `TFAIL`/`TBROK`. The returned-count increase from 92 to 93 is a
real +1 for this capped LA LTP window.

## Remaining Known Gaps

- `dirtypipe` still fails at `splice failed`; pipe capacity alone does not solve
  the splice path.
- `epoll_wait07` still has an EPOLLONESHOT failure.
- `epoll_wait03` still has an internal invalid-user-events TFAIL.
- `epoll_pwait03` still reaches the long 1s sleep segment and watchdogs in the
  full cap350 profile.

## Notes

Mutagen `oskernel-sync` was checked after the run and reported:

```text
Status: Watching for changes
```

No transition problems were present at the end of this batch.
