# TASK-021 Multigroup Watchdogs Review

Date: 2026-06-11 CST
Executor: Hermes/Codex on WSL

## Summary

TASK-021 unlocked serial execution for every discovered `*_testcode.sh` group,
with deterministic ordering, per-group wall-clock watchdogs, a global deadline,
between-group task reaping, marker repair on watchdog kill, and ENOSYS
hardening for unknown syscall numbers.

No judge submission was performed.

## Asset Inventory

Inventory was taken with `debugfs` on temporary image copies after `e2fsck -fy`
where the journal or bitmap state required recovery.

Repo-root generated/basic images:

| Image | `/glibc` scripts | `/musl` scripts | Executables observed |
|---|---|---|---|
| `sdcard-rv.img` | `basic_testcode.sh` | `basic_testcode.sh` | `busybox` under both libc dirs |
| `sdcard-la.img` | `basic_testcode.sh` | `basic_testcode.sh` | `busybox`, `libc.so` under both libc dirs |

Captured richer images:

| Image | Groups in both `/glibc` and `/musl` | LTP present? |
|---|---|---|
| `.m0-baselines/starry-next/sdcard-riscv64.img` | `basic`, `busybox`, `lua`, `libctest`, `libcbench`, `iozone`, `lmbench`, `cyclictest`, `unixbench`, `iperf`, `netperf` | no |
| `.m0-baselines/starry-next/sdcard-loongarch64.img` | same as RISC-V | no |

Representative binaries present in each rich libc directory: `busybox`, `lua`,
`entry-dynamic.exe`, `entry-static.exe`, `runtest.exe`, `iozone`, `iperf3`,
`netperf`, `netserver`, `libc-bench`, `lmbench_all`, `cyclictest`,
`hackbench`, UnixBench binaries (`arithoh`, `context1`, `dhry2`, `fstime`,
`pipe`, `spawn`, `syscall`, `whetstone-double`, etc.), and lmbench wrappers
(`lat_*`, `bw_*`, `lmdd`, `mhz`, `stream`, etc.).

Note: captured `netperf_testcode.sh` has a bad trailing marker line that prints
`START unixbench-*` instead of `END netperf-*`. The kernel preserves real script
output and repairs only the group boundary when initproc owns the boundary.

## Group Order And Budgets

Order:

`basic`, `busybox`, `lua`, `libctest`, `libcbench`, `iozone`, `lmbench`,
`cyclictest`, `unixbench`, `iperf`, `netperf`, `ltp`.

Within each group: `/glibc` copy first, then `/musl`.

Budgets:

| Group | Budget seconds |
|---|---:|
| basic | 90 |
| busybox | 45 |
| lua | 30 |
| libctest | 30 |
| libcbench | 60 |
| iozone | 30 |
| lmbench | 30 |
| cyclictest | 30 |
| unixbench | 30 |
| iperf | 1 |
| netperf | 1 |
| ltp | 60 |

Global initproc deadline: 840 seconds.

Network budgets are intentionally short. A 5 second iperf watchdog reached a
TCP path that panicked on a large allocation; 1 second still starts the real
scripts and prints real body output while preserving panic-free serial progress.

## Measured Runtimes

Final RISC-V full run:

| Group | Runtime/status |
|---|---|
| basic-glibc | 2s exited 0 |
| basic-musl | 1s exited 0 |
| busybox-glibc | 3s exited 0 |
| busybox-musl | 2s exited 0 |
| lua-glibc | 1s exited 0 |
| lua-musl | 0s exited 0 |
| libctest-glibc | 30s timed-out |
| libctest-musl | 30s timed-out |
| libcbench-glibc | 20s exited 0 |
| libcbench-musl | 2s exited 0 |
| iozone-glibc | 0s exited 0 |
| iozone-musl | 30s timed-out |
| lmbench-glibc | 30s timed-out |
| lmbench-musl | 30s timed-out |
| cyclictest-glibc | 0s exited 0 |
| cyclictest-musl | 0s exited 0 |
| unixbench-glibc | 30s timed-out |
| unixbench-musl | 30s timed-out |
| iperf-glibc | 1s timed-out |
| iperf-musl | 1s timed-out |
| netperf-glibc | 0s exited 0 |
| netperf-musl | 1s timed-out |

Final LoongArch full run:

| Group | Runtime/status |
|---|---|
| basic-glibc | 2s exited 0 |
| basic-musl | 2s exited 0 |
| busybox-glibc | 4s exited 0 |
| busybox-musl | 3s exited 0 |
| lua-glibc | 2s exited 0 |
| lua-musl | 1s exited 0 |
| libctest-glibc | 30s timed-out |
| libctest-musl | 30s timed-out |
| libcbench-glibc | 0s exited 0 |
| libcbench-musl | 60s timed-out |
| iozone-glibc | 30s timed-out |
| iozone-musl | 30s timed-out |
| lmbench-glibc | 30s timed-out |
| lmbench-musl | 30s timed-out |
| cyclictest-glibc | 0s exited 0 |
| cyclictest-musl | 30s timed-out |
| unixbench-glibc | 30s timed-out |
| unixbench-musl | 30s timed-out |
| iperf-glibc | 1s timed-out |
| iperf-musl | 1s timed-out |
| netperf-glibc | 0s exited 0 |
| netperf-musl | 1s timed-out |

Both final full runs exited QEMU with status 0 and passed the multigroup
harness. There were zero kernel panic/page-fault lines.

## Syscall And Panic Hardening

Unknown syscall dispatch now returns `-ENOSYS` and logs each number once.
Final full RISC-V and LoongArch logs contained no `unsupported syscall` lines,
so no unknown syscall numbers were hit in the measured runs.

Additional hardening applied:

- `/dev` `umount` returns `ENOSYS` instead of `todo!()`.
- `getdents64` on an invalid fd returns `EBADF` instead of unwrapping.
- `clear_child_tid` is best-effort when the address is unmapped.
- fd table getters/setters check the real table length.
- high-address page faults are routed to the current user task when the current
  task is a `UserTask`; kernel-context high faults still panic.
- socket drop/shutdown/send/poll paths avoid known unwrap/expect panics reached
  by watchdog-killed network tasks.
- executor skips and removes already-exited user tasks instead of polling or
  requeueing killed futures.

Remaining audit notes for later tasks: `rg` still finds some `unwrap`/`expect`
and two `todo!`/`unimplemented!` sites in less directly exercised code paths
(`RAW` socket reuse and unknown ELF type). They were not hit by final full runs.

## Verification Commands

Required basic regression command:

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'export RUSTUP_TOOLCHAIN=nightly-2025-01-18 CARGO_NET_OFFLINE=true; make all; file kernel-rv kernel-la; TIMEOUT_SECONDS=900 ./harness/run-rv.sh; TIMEOUT_SECONDS=900 ./harness/run-la.sh'
```

Result: `kernel-rv` and `kernel-la` are ELF files; RISC-V and LoongArch basic
harnesses pass.

Required follow-up clean archive command:

```sh
docker run --rm --network none -v /tmp/task021-clean-0NP2rc/oskernel2025-x:/workspace -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'set -euo pipefail; export CARGO_HOME=/tmp/task021-empty-cargo-home; rm -rf "$CARGO_HOME"; mkdir -p "$CARGO_HOME"; make all; file kernel-rv kernel-la; file kernel-rv kernel-la | grep -Eq "kernel-rv: .*ELF"; file kernel-rv kernel-la | grep -Eq "kernel-la: .*ELF"'
```

Setup: `/tmp/task021-clean-0NP2rc/oskernel2025-x` was created from
`git archive HEAD`, the final TASK-021 patch, and the untracked TASK-021 review
packet. It had no `target/`, `kernel-rv`, `kernel-la`, or `kernel/mount.img`
before the build, and the container used an empty `CARGO_HOME`.

Result: passed. Clean archive `make all` built both architectures with
`--network none`; `file` reported `kernel-rv` as RISC-V ELF and `kernel-la` as
LoongArch ELF.

Final RISC-V multigroup command:

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'TEST_IMAGE=/workspace/.m0-baselines/starry-next/sdcard-riscv64.img TEST_IMAGE_LAYOUT=nested TIMEOUT_SECONDS=900 EXPECTED_GROUPS=basic-glibc,basic-musl,busybox-glibc,busybox-musl,lua-glibc,lua-musl,libctest-glibc,libctest-musl,libcbench-glibc,libcbench-musl,iozone-glibc,iozone-musl,lmbench-glibc,lmbench-musl,cyclictest-glibc,cyclictest-musl,unixbench-glibc,unixbench-musl,iperf-glibc,iperf-musl,netperf-glibc,netperf-musl ./harness/run-rv.sh'
```

Result: pass.

Final LoongArch multigroup command:

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'TEST_IMAGE=/workspace/.m0-baselines/starry-next/sdcard-loongarch64.img TEST_IMAGE_LAYOUT=nested TIMEOUT_SECONDS=900 EXPECTED_GROUPS=basic-glibc,basic-musl,busybox-glibc,busybox-musl,lua-glibc,lua-musl,libctest-glibc,libctest-musl,libcbench-glibc,libcbench-musl,iozone-glibc,iozone-musl,lmbench-glibc,lmbench-musl,cyclictest-glibc,cyclictest-musl,unixbench-glibc,unixbench-musl,iperf-glibc,iperf-musl,netperf-glibc,netperf-musl ./harness/run-la.sh'
```

Result: pass.

## First Follow-Up Failures

- busybox: all observed busybox testcase lines were `success` in final logs;
  TASK-022 should still produce the required per-command table.
- lua: all observed lua testcase lines were `success`; TASK-023 should lock the
  36/36 table.
- libctest: both libc dirs time out at 30s; output reaches `fwscanf` and
  `pthread_cancel_points` with file/thread behavior gaps.
- libcbench: RISC-V completes; LoongArch musl times out during
  `b_malloc_thread_stress`.
- iozone: RISC-V musl and both LoongArch groups time out; RISC-V shows ext4
  I/O errors in throughput modes.
- lmbench: both architectures and libcs time out after initial latency output.
- cyclictest: no-stress/stress runs print `end: fail`; LoongArch musl times out.
- unixbench: both architectures and libcs time out.
- iperf: daemonization reports `unable to become a daemon: No such device`, then
  groups are watchdog-killed at 1s.
- netperf: glibc runs fail on `libm.so.6` version mismatches; musl netserver
  reports `/dev/null debug file : Not a directory` and times out.
- ltp: no `ltp_testcode.sh` was present in the available rich captured images.
