# LTP epoll EPOLLONESHOT Progress

Date: 2026-06-16 CST.

## Scope

This packet covers the `EPOLLONESHOT` follow-up after the earlier fd/pipe/epoll
batch.

The honest scoring result from this follow-up is:

```text
LA ltp-glibc cap350: 93 returned 0 -> 93 returned 0
Delta: +0 returned-count cases
```

`epoll_wait07` now has no internal LTP `TFAIL`, but it already returned process
exit code 0 in the previous full cap350 run. This is a correctness cleanup, not
a returned-count gain.

## Root Cause Addressed

`epoll_wait07` showed that `EPOLLONESHOT` registrations were not disabled after
delivering the first event. The second write to the same pipe therefore produced
another `EPOLLIN` notification, while the test expected no further event until
the interest was re-armed.

The implementation now stores epoll interests as:

```text
EpollInterest { event, last_ready, disabled }
```

`EPOLL_CTL_ADD` creates enabled interests, `EPOLL_CTL_MOD` re-arms them, and
`epoll_wait` skips disabled interests. After an event is actually delivered, an
interest carrying `EPOLLONESHOT` is marked disabled.

## Fresh Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

### Targeted RED

Log:

```text
/tmp/la-epolloneshot-red.log
```

Result before the fix:

```text
RUN LTP CASE 000_epoll_wait07
epoll_wait07.c:53: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 1 (1)
epoll_wait07.c:58: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 0 (0)
epoll_wait07.c:63: TFAIL: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) (1) != 0 (0)
FAIL LTP CASE 000_epoll_wait07 : 0
initproc ltp: attempted 3 of 2843 cases, 2 returned 0
```

### Targeted GREEN

Log:

```text
/tmp/la-epolloneshot-green.log
```

Result after the fix:

```text
RUN LTP CASE 000_epoll_wait07
epoll_wait07.c:53: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 1 (1)
epoll_wait07.c:58: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 0 (0)
epoll_wait07.c:63: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 0 (0)
FAIL LTP CASE 000_epoll_wait07 : 0
initproc ltp: attempted 3 of 2843 cases, 2 returned 0
```

The same targeted run kept `epoll_wait06` green:

```text
RUN LTP CASE 000_epoll_wait06
epoll_wait06.c:80: TPASS: write() failed : EAGAIN/EWOULDBLOCK (11)
epoll_wait06.c:82: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 1 (1)
epoll_wait06.c:91: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 0 (0)
epoll_wait06.c:98: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 1, 0) == 1 (1)
epoll_wait06.c:100: TPASS: evt_receive.events & EPOLLOUT == EPOLLOUT (4)
FAIL LTP CASE 000_epoll_wait06 : 0
```

### Clean Build

Log:

```text
.codex-logs/clean-build-epolloneshot-20260616.log
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
.codex-logs/rv-basic-epolloneshot-20260616.log
.codex-logs/la-basic-epolloneshot-20260616.log
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
/tmp/la-cap350-after-pipesz-epollet.log
```

Previous result:

```text
RUN LTP CASE epoll_wait07
epoll_wait07.c:53: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 1 (1)
epoll_wait07.c:58: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 0 (0)
epoll_wait07.c:63: TFAIL: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) (1) != 0 (0)
FAIL LTP CASE epoll_wait07 : 0
initproc ltp: attempted 350 of 2840 cases, 93 returned 0
```

Current log:

```text
/tmp/la-cap350-after-epolloneshot.log
```

Current result:

```text
RUN LTP CASE epoll_wait07
epoll_wait07.c:53: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 1 (1)
epoll_wait07.c:58: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 0 (0)
epoll_wait07.c:63: TPASS: SAFE_EPOLL_WAIT(epfd, &evt_receive, 10, 0) == 0 (0)
FAIL LTP CASE epoll_wait07 : 0
initproc ltp: attempted 350 of 2840 cases, 93 returned 0
```

Summary counters:

```text
previous cap350: RUN=350, TPASS=619, TFAIL=156, TBROK=71
current  cap350: RUN=350, TPASS=620, TFAIL=155, TBROK=71
```

`epoll_wait06` remained internally green in the current cap350 run.

## Remaining Known Gaps

- This follow-up does not add returned-count growth because `epoll_wait07`
  already exited with code 0 in the previous full cap350 run.
- `epoll_wait03` still has an invalid-user-events internal `TFAIL`.
- `epoll_pwait03` still watchdogs in the full cap350 profile.
- `dirtypipe` still fails at `splice failed`; this follow-up does not change the
  splice path.
