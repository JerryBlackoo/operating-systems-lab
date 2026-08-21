# TASK-027 LTP Sweep Batch 1

Date: 2026-06-14 CST
Executor: WSL Codex

## Scope

TASK-027 produced the first named, freeze-free LTP walk profile for all four
platform-runs, recorded the baseline/post-fix pass counts and exit histograms,
and landed the first cheap kernel-side batch:

- kernel-side LTP case walker with lexical case names, per-case watchdog, case
  count truncation markers, and known crash-storm quarantine;
- runtime rootfs prep for `/tmp`, `/dev/shm`, `/etc`, `/bin` applets, and LTP
  path/lib search;
- low-risk syscall/procfs/file semantics for identity/capability stubs, xattr
  graceful errors, `prctl`, `adjtimex` no-op, `fallocate`, memory-policy no-ops,
  `O_CREAT|O_EXCL`, and bounded `ppoll`;
- RV/LA trap containment so user faults become task failures instead of kernel
  panics.

## Inventory

Inventory source images:

- `/home/lenovo/Downloads/oscomp-sdcard/sdcard-rv.img`
- `/home/lenovo/Downloads/oscomp-sdcard/sdcard-la.img`

Inventory files are under `/tmp/task027-ltp/inventory/`.

| platform-run | `ltp/testcases/bin` entries excluding dot | regular files walked | first 12 lexical names |
| --- | ---: | ---: | --- |
| rv-musl | 2822 | 2820 | abort01, abs01, accept01, accept02, accept03, accept4_01, access01, access02, access03, access04, acct01, acct02 |
| la-musl | 2822 | 2820 | abort01, abs01, accept01, accept02, accept03, accept4_01, access01, access02, access03, access04, acct01, acct02 |
| rv-glibc | 2842 | 2840 | abort01, abs01, accept01, accept02, accept03, accept4_01, access01, access02, access03, access04, acct01, acct02 |
| la-glibc | 2842 | 2840 | abort01, abs01, accept01, accept02, accept03, accept4_01, access01, access02, access03, access04, acct01, acct02 |

The kernel walker filters regular files, drops dot entries, and sorts by Rust
`String::sort()` lexical order. That matches the inventory sorted files.

## Baseline Sweep

Baseline logs are in `/tmp/task027-ltp/logs/`.

| platform-run | log | attempted | `: 0` | last named case | wall-clock | harness | truncation/freeze |
| --- | --- | ---: | ---: | --- | ---: | --- | --- |
| rv-musl | `rv-musl-baseline.log` | 221 | 33 | crash02 | 579s | status 7 stall | stalled after crash02, no END |
| la-musl | `la-musl-baseline.log` | 0 | 0 | none | 520s | status 7 stall | stalled before named RUN output |
| rv-glibc | `rv-glibc-baseline.log` | 241 | 22 | crash02 | 621s | status 7 stall | stalled after crash02, no END |
| la-glibc | `la-glibc-baseline.log` | 0 | 0 | none | 403s | status 7 stall | stalled before named RUN output |

Baseline exit histograms:

| platform-run | histogram |
| --- | --- |
| rv-musl | 0:33, 1:12, 2:98, 3:1, 4:2, 6:4, 22:1, 32:45, 124:22, 128:1, 139:1 |
| la-musl | none; no named case completed before stall |
| rv-glibc | 0:22, 1:13, 2:86, 3:1, 4:1, 5:1, 6:4, 22:1, 32:28, 124:23, 127:1, 128:1, 139:57 |
| la-glibc | none; no named case completed before stall |

## Post-Fix Sweep

Final images:

- `/tmp/task027-ltp/final-images/rv-musl-ltp-final.img`
- `/tmp/task027-ltp/final-images/la-musl-ltp-final.img`
- `/tmp/task027-ltp/final-images/rv-glibc-ltp-final.img`
- `/tmp/task027-ltp/final-images/la-glibc-ltp-final.img`

Each image carries `.ltp_case_limit=350`. The truncation is intentional and is
printed by the kernel as a truthful profile/budget limit.

| platform-run | log | attempted | `: 0` | total regular | truncation point | guest LTP time | host wall | harness |
| --- | --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| rv-musl | `rv-musl-final-cap350-qfinal.log` | 350 | 76 | 2820 | before execve04 | 559s | 702s | status 0, no stall/panic |
| la-musl | `la-musl-final-cap350-qfinal.log` | 350 | 75 | 2820 | before execve04 | 553s | 555s | status 0, no stall/panic |
| rv-glibc | `rv-glibc-final-cap350-q4.log` | 350 | 8 | 2840 | before eventfd05 | 693s | 872s | status 0, no stall/panic |
| la-glibc | `la-glibc-final-cap350-q2.log` | 350 | 42 | 2840 | before eventfd05 | 747s | 748s | status 0, no stall/panic |

Post-fix exit histograms:

| platform-run | histogram |
| --- | --- |
| rv-musl | 0:76, 1:14, 2:103, 3:2, 4:5, 22:1, 32:95, 34:5, 36:5, 124:34, 127:1, 128:1, 132:1, 139:6, 255:1 |
| la-musl | 0:75, 1:15, 2:103, 3:2, 4:5, 22:1, 32:73, 34:5, 36:3, 124:33, 128:1, 132:1, 139:32, 255:1 |
| rv-glibc | 0:8, 1:14, 2:36, 3:2, 4:3, 5:1, 22:1, 32:24, 124:44, 127:1, 128:1, 139:212, 255:1 |
| la-glibc | 0:42, 1:38, 2:23, 3:1, 4:5, 7:3, 32:15, 34:2, 38:1, 124:48, 127:2, 128:1, 132:6, 139:161, 255:1 |

Pass-count delta:

| platform-run | baseline `: 0` | post-fix `: 0` | delta |
| --- | ---: | ---: | ---: |
| rv-musl | 33 | 76 | +43 |
| la-musl | 0 | 75 | +75 |
| rv-glibc | 22 | 8 | -14 |
| la-glibc | 0 | 42 | +42 |

The acceptance target for musl-rv is `>= +100 cases or 2x baseline, whichever
is smaller`. Baseline 33 makes the effective threshold 66 post-fix passes.
Post-fix rv-musl reached 76.

## Cluster Table

| cluster | examples from logs | current kernel gap | estimated remaining yield |
| --- | --- | --- | ---: |
| LTP runner survivability/crash storms | crash01, crash02, clone02, dma_thread_diotest | upstream scripts could wedge the executor; crash-storm cases need quarantine until kill/reap is stronger | already unlocked all four capped walks; remaining yield is coverage, not direct pass count |
| glibc LA noisy mount/cgroup loops | la-glibc `umount(mntpoint/ovl) failed: EINVAL`, `cgroup.procs` ENOENT repeats | tmpfs/cgroup/mount semantics are too shallow and can print huge logs | 20-60 in glibc early window plus major time savings |
| clock/timer/time adjust | clock_settime01/02, clock_settime03, adjtimex | `clock_settime`, timer create, clock error semantics incomplete; `adjtimex` now no-op but tests expect stricter behavior | 15-30 |
| credentials, uid/gid, capabilities | chown/chmod/access/chroot cases, capget/capset, `/etc/passwd` lookups | identity stubs and `/etc` seed improved cheap paths; real ownership/mode persistence and permission errno remain incomplete | 20-50 |
| procfs/kernel config/sysctl | `/proc/sys/kernel/tainted`, `pid_max`, kernel `.config`, `/proc/cpuinfo` | procfs has only narrow static files; missing sysctl tree and config.gz shape | 20-40 |
| filesystem metadata and xattr/statfs | chmod/chown mode checks, xattr ENODATA paths, statfs/statvfs, mkfifo | xattr graceful errors added; real mode/uid/gid, FIFO, statfs details remain incomplete | 30-70 |
| IPC/fork/wait/exec resources | `LTP_IPC_PATH is not defined`, execve child copy failures, wait4 watchdogs | no LTP IPC setup; wait/exec cleanup still relies on watchdog for hard cases | 20-60 |
| epoll/eventfd/signalfd/timerfd/pidfd | epoll_create*, epoll_wait*, eventfd01-04 | several fd families are ENOSYS or semantically thin; glibc often turns this into 139 | 40-100, especially glibc |
| socket/network feature probes | AF_ALG EINVAL, IPv6 disabled, `ip` missing, bind EAFNOSUPPORT | expected missing kernel/netlink/AF_ALG support and absent helper applets | 10-30 unless network tasks add stack coverage |
| rlimit/prlimit/resource accounting | libctest rlimit_open_files, LTP resource cases | rlimit semantics are partial and watchdog still kills some wait paths | 10-25 |
| memory mapping/bad address/brk/mincore | `mmap() failed to get bad address`, brk not implemented notices, mincore no-op | mmap invalid-address behavior and brk accounting are incomplete | 20-50 |

Batch #2 should prioritize glibc 139 storms and high-yield early-window groups:
epoll/eventfd basics, tmpfs/mount/cgroup quiet failures, procfs sysctl stubs,
and real chmod/chown mode persistence.

## Regression Verification

Official image build and regression logs:

- `/tmp/task027-ltp/regression/build-basic.log`
- `/tmp/task027-ltp/regression/busybox-lua.log`
- `/tmp/task027-ltp/regression/libctest.log`

Results:

- `make -B all` in `zhouzhouyi/os-contest:20260510` with `--network none`
  produced ELF `kernel-rv` and `kernel-la`.
- basic RV and LA passed.
- busybox/lua RV and LA passed harness markers and real output.
- libctest-musl RV and LA passed harness markers and real output; known long
  cases were watchdog-contained with no stall/panic.

No judge submission was performed.
