# TASK-023 Watchdog And Budget Profile

Date: 2026-06-11. Executor: WSL Codex. Ground truth image:
`zhouzhouyi/os-contest:20260510`.

## Summary

Implemented a three-layer freeze defense:

- Per-task wall-clock deadline inherited by child user tasks spawned while a
  group script runs.
- Per-group watchdog with heartbeat output every 60s for legitimately quiet
  long-running groups.
- Global deadline enforced from the timer interrupt path, not from `initproc`.

Also added harness output-stall detection. The harness now streams QEMU output
and fails if the console log stops growing for more than `STALL_TIMEOUT_SECONDS`
(default 120s).

## Freeze Root Cause Status

The 5657cca OJ symptom is consistent with mechanism class (a): executor
starvation or a kernel path that prevented `initproc` from running, because the
old global deadline lived inside `initproc` and never fired.

Exact LTP case name could not be identified locally: the available copied full
images under `.m0-baselines/starry-next/` contain basic/busybox/lua/libctest and
perf/network scripts, but no `ltp/testcases/bin` tree and no `ltp_testcode.sh`.
After repairing only the `/tmp/task023-la-full.img` copy with `e2fsck`, `debugfs`
confirmed `/glibc` and `/musl` have no LTP payload. The upstream captured script
exists at `contest_capture/raw/github/scripts/ltp/ltp_testcode.sh`, but the case
binaries needed to walk order are absent.

Local evidence from the available rich image did reproduce the hang families
that previously consumed group budgets:

- `libctest-musl` pthread-cancel cases were killed by per-task limits and the
  script continued to later cases.
- bounded RV/LA full-image runs showed no 120s harness stall after heartbeat
  output was added.

## Fixed Kernel Wait Loops

Audited syscall-facing cooperative waits:

- `wait4`: `WaitPid` now returns on killed waiter.
- `futex`: wait path checks killed state before and after waiting; `WaitFutex`
  removes killed tasks from futex queues and returns `EINTR`.
- `nanosleep` / `clock_nanosleep`: wait futures are kill-aware.
- `poll` / `ppoll`: loop checks killed state.
- `select` / `pselect`: loop checks killed state.
- `epoll_pwait`: loop checks killed state.
- socket `accept` / `accept4` / `connect` / `recvfrom`: loops check killed
  state.
- signal waits `sigsuspend` / `pause`: loops check killed state.
- async signal waits: `WaitSignal` / `WaitHandleAbleSignal` wake on killed task.

Pipe and tty blocking reads/writes are implemented as pending futures in the fs
crate. They do not spin inside one poll; killed user tasks are dropped by the
executor before their syscall future can continue, and child wall deadlines are
checked in `UserTask::before_run()`.

## Global Deadline Design

`kernel/kernel/src/watchdog.rs` stores an atomic absolute deadline. The timer
interrupt handler calls `watchdog::check_global_deadline_from_timer()` after
programming the next tick. If the deadline has passed, it prints one watchdog
line and calls `shutdown()` directly.

This cannot starve with `initproc` because it is not an async task and does not
depend on the cooperative task queue. It still depends on timer interrupts being
delivered; that is the right layer for this kernel because a task that merely
fails to yield cannot stop the interrupt handler.

Default global deadline is 6600s per architecture. A local-only test-image file
`/.watchdog_global_deadline_secs` can override it for synthetic probes; judge
images do not contain that file.

## Budget Profile A

Per libc directory, per architecture:

| Group | Budget | Per-task limit | Status |
|---|---:|---:|---|
| basic | 180s | 20s | run |
| busybox | 120s | 10s | run |
| lua | 60s | 10s | run |
| libctest-musl | 600s | 20s | run |
| libctest-glibc | skipped | n/a | skipped, no markers |
| libcbench | 240s | 600s | run |
| iozone | 240s | 600s | run |
| lmbench | 300s | 600s | run |
| cyclictest | 90s | 600s | run |
| iperf | 90s | 30s | run |
| netperf | 90s | 30s | run |
| unixbench | skipped | n/a | skipped, no markers |
| ltp | 1200s | 15s | run |

Worst-case per architecture is 6510s:
`basic 360 + busybox 240 + lua 120 + libctest-musl 600 + libcbench 480 +
iozone 480 + lmbench 600 + cyclictest 180 + iperf 180 + netperf 180 + ltp 2400`.
This leaves 690s headroom under the 7200s judge cap.

## Verification

Build and basic harness:

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace \
  zhouzhouyi/os-contest:20260510 bash -lc \
  'export RUSTUP_TOOLCHAIN=nightly-2025-01-18 CARGO_NET_OFFLINE=true; \
   make -B all; file kernel-rv kernel-la'
```

Result: passed. Produced RISC-V and LoongArch ELF kernels.

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace \
  zhouzhouyi/os-contest:20260510 bash -lc \
  'export RUSTUP_TOOLCHAIN=nightly-2025-01-18 CARGO_NET_OFFLINE=true; \
   TIMEOUT_SECONDS=1200 ./harness/run-rv.sh >/tmp/rv-basic.log 2>&1; \
   rv=$?; TIMEOUT_SECONDS=1200 ./harness/run-la.sh >/tmp/la-basic.log 2>&1; \
   la=$?; echo rv=$rv la=$la; tail -5 /tmp/rv-basic.log; \
   tail -5 /tmp/la-basic.log; exit $((rv || la))'
```

Result: passed. `rv=0 la=0`; both printed `PASS: ... basic markers and real
basic case output found`.

Synthetic local-only probes:

- Per-task spin and fork storm:
  `TEST_IMAGE=/tmp/task023-rv-probes.img TEST_IMAGE_LAYOUT=flat
  EXPECTED_GROUPS=libctest TIMEOUT_SECONDS=120 STALL_TIMEOUT_SECONDS=30
  ./harness/run-rv.sh`.
  Result: passed. Log showed child task kills and script continuation:
  `probe spin-status 124`, `probe fork-status 124`, `probe sleep-status 0`,
  `PASS: RISC-V libctest markers...`.
- Per-group watchdog:
  `TEST_IMAGE=/tmp/task023-group.img TEST_IMAGE_LAYOUT=flat
  EXPECTED_GROUPS=cyclictest TIMEOUT_SECONDS=140 STALL_TIMEOUT_SECONDS=120
  ./harness/run-rv.sh`.
  Result: passed. Log showed heartbeat at 60s, command timeout at 90s,
  repaired END marker, and harness PASS.
- Global timer-interrupt deadline:
  `TEST_IMAGE=/tmp/task023-global.img TEST_IMAGE_LAYOUT=flat
  EXPECTED_GROUPS=basic TIMEOUT_SECONDS=40 STALL_TIMEOUT_SECONDS=20
  ./harness/run-rv.sh`.
  Result: expected harness failure `status=4` because shutdown fired before the
  group END marker. Log contained:
  `kernel watchdog: global deadline reached from timer interrupt; forcing
  shutdown`.
- Harness stall detector:
  `TEST_IMAGE=/tmp/task023-stall.img TEST_IMAGE_LAYOUT=flat
  EXPECTED_GROUPS=cyclictest TIMEOUT_SECONDS=60 STALL_TIMEOUT_SECONDS=5
  ./harness/run-rv.sh`.
  Result: expected harness failure `status=7`; log contained
  `HARNESS STALL: no console output for 5s`.

Bounded rich-image runs:

- LA copied full image, 360s cap: reached `libcbench-musl`; per-task limits
  killed libctest hangs and libctest completed. Earlier run without heartbeat
  proved the old harness stall detector would fire on a quiet long group; after
  heartbeat this class is covered.
- RV copied full image, 240s cap: reached `iozone-musl`; no freeze or harness
  stall, heartbeat printed `initproc watchdog: group iozone-musl still running
  at 60s`. The outer `TIMEOUT_SECONDS=240` ended the bounded run before full
  profile completion.

## Remaining Blockers

- Exact OJ LTP wedging case remains unknown because this workspace lacks the
  LoongArch judge-like LTP sdcard payload. Need the actual judge image or a
  copied full LTP rootfs to walk `ltp/testcases/bin` order and bisect.
- Full profile-A end-to-end runs were not completed locally within this turn;
  bounded runs and synthetic probes passed, but TASK-023 acceptance should keep
  OJ submission blocked until a true LTP-containing image is available or a full
  run is performed.

## Follow-Up Artifact Search

Date: 2026-06-11 17:58 CST. Follow-up run:
`TASK-023-20260611-175312`.

Real LTP payload found: no.

Searched locations:

- Workspace artifacts: `.m0-baselines`, `.hermes`, `contest_capture`,
  `review_packets`, `docs`, repo-root ignored images, and `kernel/mount.img`.
- Downloads and copied artifacts already present on WSL:
  `/home/lenovo/Downloads`, `/home/lenovo/downloads`, `/home/lenovo/下载`,
  `/home/lenovo` sibling workspace copies, and `/tmp` task workdirs.
- WSL-accessible Windows user files under `/mnt/c/Users` to depth 5.
- Official image, offline:
  `docker run --rm --network none zhouzhouyi/os-contest:20260510 ...`.

Image candidates inspected with `debugfs` after copying and `e2fsck -fy` where
journal recovery was needed:

- `/home/lenovo/oskernel2025-x/sdcard-rv.img`
- `/home/lenovo/oskernel2025-x/sdcard-la.img`
- `/home/lenovo/oskernel2025-x/kernel/mount.img`
- `/home/lenovo/oskernel2025-x/.hermes/task022/focus/la-busybox-glibc.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/ByteOS/mount.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/StarryOS/make/disk.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/StarryOS/rootfs-loongarch64.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/StarryOS/rootfs-riscv64.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/starry-next/.arceos/disk.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/starry-next/sdcard-loongarch64.img`
- `/home/lenovo/oskernel2025-x/.m0-baselines/starry-next/sdcard-riscv64.img`
- `/home/lenovo/oskernel2025-x-task018/sdcard-rv.img`
- `/home/lenovo/oskernel2025-x-task018/sdcard-la.img`
- `/home/lenovo/oskernel2025-x-task018/kernel/mount.img`
- `/home/lenovo/oskernel2025-x-task019/sdcard-rv.img`
- `/home/lenovo/oskernel2025-x-task019/sdcard-la.img`
- `/home/lenovo/oskernel2025-x-task019/kernel/mount.img`
- `/tmp/oskernel-task020/sdcard-la.img`
- `/tmp/oskernel-task020/sdcard-rv.img`
- `/tmp/task018-clean/sdcard-la.img`
- `/tmp/task018-clean/sdcard-rv.img`
- `/tmp/task021-inventory/sdcard-la.img`
- `/tmp/task021-inventory/sdcard-rv.img`
- `/tmp/task021-inventory/sdcard-loongarch64.img`
- `/tmp/task021-inventory/sdcard-riscv64.img`
- `/tmp/task023-la-full.img`
- `/tmp/task023-rv-full.img`
- `/tmp/task023-global.img`
- `/tmp/task023-group.img`
- `/tmp/task023-stall.img`
- `/tmp/task023-rv-probes.img`

Negative evidence:

- None of the inspected ext images had non-empty directory entries for `/ltp`,
  `/ltp/testcases/bin`, `/glibc/ltp/testcases/bin`, or
  `/musl/ltp/testcases/bin`.
- None of the inspected ext images had `/ltp_testcode.sh`,
  `/glibc/ltp_testcode.sh`, or `/musl/ltp_testcode.sh` by `debugfs stat`.
- Archive scans found no LTP payload in
  `.m0-baselines/byteos-deps.tar.gz` or `/tmp/task021-clean-0NP2rc/untracked.tar`.
- The official image search found no `/coursegrader/testdata/sdcard-*.img.gz`
  and no local LTP payload.
- The only local LTP artifact remains the captured script
  `contest_capture/raw/github/scripts/ltp/ltp_testcode.sh`; it does not include
  the `ltp/testcases/bin` binaries needed to identify the 5657cca wedging case
  or run full profile-A LTP acceptance.

Blocker: TASK-023 is blocked on a true LTP-containing judge-like sdcard/rootfs.
The user must provide an artifact containing the real `/glibc/ltp/testcases/bin`
and `/musl/ltp/testcases/bin` trees before the wedging case can be identified
and full profile-A RV/LA acceptance runs can be completed.
