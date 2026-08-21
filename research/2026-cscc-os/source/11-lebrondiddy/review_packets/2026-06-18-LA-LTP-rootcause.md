# 2026-06-18 LA LTP root-cause packet

Date: 2026-06-18
Prepared by: Codex

## Scope

This packet diagnoses the LA LTP "0 / early death" question from faithful full
profile replays. No kernel functional code was changed for this packet.

Counting rule used throughout: only real LTP case exit code 0 was counted, via
`FAIL LTP CASE ... : 0` or the guest `returned 0` summary. Test-output garbage
containing `0` was not counted.

## Short Answer

| Quadrant | Three-way classification | Evidence-backed interpretation |
| --- | --- | --- |
| `glibc-la` | Global timeout / result aggregation, not local glibc LTP death | Local faithful replay completed `ltp-glibc`: `2840/2840` attempted, `226 returned 0`, END marker present. It was not a crash and not an sdcard mismatch. If OJ reports `glibc-la = 0`, the plausible explanation is that the later LA full-run global shutdown caused the judge/aggregator to discard or miss already printed glibc LTP results. |
| `musl-la` | Global timeout / budget truncation | Local faithful replay entered `ltp-musl`, produced real zeros, then hit the kernel global deadline during `fs_bind_rbind20.sh`. Observed before shutdown: `626` runs, `150 returned 0`, `42` case timeouts. It was not "never reached"; it was truncated mid-group. |

The two excluded options are well supported:

- **Crash:** excluded in the faithful LA run. Crash grep count was `0`, and the
  terminal line is `kernel watchdog: global deadline reached from timer interrupt; forcing shutdown`.
- **sdcard inconsistency:** excluded as the primary cause. The faithful run used
  the staged full 4G LA image, and copied/fsck-inspected images contain the full
  `/glibc` and `/musl` test directories, including all 12 `*_testcode.sh` files.

## Faithful Replay Evidence

Artifacts:

```text
/home/yxzhang/oskernel-artifacts/rootcause-20260618/faithful-la-full-7200.log
/home/yxzhang/oskernel-artifacts/rootcause-20260618/faithful-la-full-7200-driver.log
/home/yxzhang/oskernel-artifacts/rootcause-20260618/faithful-la-full-7200.img

/home/yxzhang/oskernel-artifacts/rootcause-20260618/faithful-rv-full-7200.log
/home/yxzhang/oskernel-artifacts/rootcause-20260618/faithful-rv-full-7200-driver.log
/home/yxzhang/oskernel-artifacts/rootcause-20260618/faithful-rv-full-7200.img
```

LA driver:

```text
HOST_START 2026-06-18T05:00:06+00:00
ARCH la
QEMU_VERSION QEMU emulator version 10.0.2
KERNEL /workspace/kernel-la: ELF 64-bit LSB executable, LoongArch, statically linked
IMAGE_STAT /art/faithful-la-full-7200.img 4294967296 ...
HOST_END 2026-06-18T06:50:09+00:00 rc=0
```

LA terminal state:

```text
2026-06-18T06:50:07+00:00 RUN LTP CASE fs_bind_rbind20.sh
2026-06-18T06:50:08+00:00 kernel watchdog: global deadline reached from timer interrupt; forcing shutdown
```

RV comparison driver:

```text
HOST_START 2026-06-18T06:52:05+00:00
ARCH rv
QEMU_VERSION QEMU emulator version 10.0.2
KERNEL /workspace/kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
IMAGE_STAT /art/faithful-rv-full-7200.img 4294967296 ...
HOST_END 2026-06-18T08:52:05+00:00 rc=124
```

RV terminal state:

```text
2026-06-18T08:52:03+00:00 RUN LTP CASE if-addr-adddel.sh
2026-06-18T08:52:05+00:00 qemu-system-riscv64: terminating on signal 15 from pid 24 (timeout)
```

RV did not show a kernel global-deadline line before the outer 7200s host
timeout killed QEMU.

Crash scan:

```text
LA panic/page-error/page-fault count: 0
RV panic/page-error/page-fault count: 0
```

## LTP Results

| Run | Terminal state | Attempted / observed runs | Real returned 0 | Case 124 timeouts | Last run |
| --- | --- | ---: | ---: | ---: | --- |
| `ltp-glibc` on LA | Complete, END marker present | `2840/2840` attempted | `226` | `92` | `zram_lib.sh` |
| `ltp-musl` on LA | Kernel global deadline mid-group | `626` observed runs | `150` | `42` | `fs_bind_rbind20.sh` |
| `ltp-glibc` on RV | Complete, END marker present | `2840/2840` attempted | `220` | `88` | `zram_lib.sh` |
| `ltp-musl` on RV | Outer host timeout mid-group | `948` observed runs | `203` | `92` | `if-addr-adddel.sh` |

Important nuance: the LA `glibc` LTP group is a positive local result, not a
local zero. The only way to reconcile an OJ `glibc-la = 0` with this replay is a
judge-side run/aggregation difference, for example discarding group output after
the later full-run forced shutdown.

## Time Budget Distribution

Current `initproc` global deadline is `6600` seconds, not `7200`; the outer QEMU
wrapper timeout in this replay was `7200` seconds.

Pre-LTP group time below uses the kernel's own `initproc watchdog: group ...
finished in Ns` lines.

| Group | LA seconds / state | RV seconds / state | LA - RV |
| --- | ---: | ---: | ---: |
| `basic-glibc` | `7`, exited | `6`, exited | `+1` |
| `basic-musl` | `5`, exited | `4`, exited | `+1` |
| `busybox-glibc` | `11`, exited | `10`, exited | `+1` |
| `busybox-musl` | `11`, exited | `10`, exited | `+1` |
| `lua-glibc` | `8`, exited | `5`, exited | `+3` |
| `lua-musl` | `6`, exited | `4`, exited | `+2` |
| `libctest-glibc` | skipped | skipped | `0` |
| `libctest-musl` | `40`, exited | `40`, exited | `0` |
| `libcbench-glibc` | `60`, timed out | `60`, timed out | `0` |
| `libcbench-musl` | `600`, timed out | `23`, exited | `+577` |
| `iozone-glibc` | `180`, timed out | `180`, timed out | `0` |
| `iozone-musl` | `197`, exited | `179`, exited | `+18` |
| `lmbench-glibc` | `540`, timed out | `133`, exited | `+407` |
| `lmbench-musl` | `117`, exited | `113`, exited | `+4` |
| `cyclictest-*` / `unixbench-*` | skipped | skipped | `0` |
| `iperf-glibc` | `4`, exited | `2`, exited | `+2` |
| `iperf-musl` | `32`, exited | `31`, exited | `+1` |
| `netperf-glibc` | `2`, exited | `1`, exited | `+1` |
| `netperf-musl` | `32`, exited | `32`, exited | `0` |
| **Pre-LTP total** | **`1852`** | **`833`** | **`+1019`** |

LTP budget consumption:

| Segment | LA | RV |
| --- | ---: | ---: |
| Pre-LTP groups | `1852s` | `833s` |
| `ltp-glibc` group | `3451s` | `2766s` |
| Budget left when `ltp-musl` starts, from `6600s` | about `1297s` | about `3001s` |
| `ltp-musl` observed before termination | about `1284s`, kernel global deadline | about `2687s`, outer host timeout |

Main LA budget sinks versus RV:

- `libcbench-musl`: LA spends the full `600s` timeout; RV exits in `23s`.
- `lmbench-glibc`: LA spends the full `540s` timeout; RV exits in `133s`.
- `ltp-glibc`: LA spends `3451s`; RV reports `2766s` from the kernel group timer.

Together, these explain why LA has only about 21 minutes left for `ltp-musl` in
the full profile.

## LA LTP Long-Tail Cases

Case-level time cost was measured by the 15s LTP watchdog result code `124`.
These are not counted as returned-zero cases; they identify where the LTP budget
is being consumed.

LA `ltp-glibc` had `92` case-level `124` timeouts. Largest families:

| Family / prefix | Timeout count |
| --- | ---: |
| `cpuset*` | `14` |
| `cpuhotplug*` | `9` |
| `cgroup*` | `8` |
| `ftp*` | `6` |
| `dns*` | `4` |
| `ftrace*` | `4` |
| `mc_*` | `4` |
| `dio*` | `3` |
| `epoll*` | `3` |
| `fork*` | `3` |
| `http*` | `3` |

Representative early `ltp-glibc` budget consumers:

```text
accept01
accept4_01
af_alg07
bind05
cgroup_fj_proc
cgroup_regression_3_1.sh
cgroup_regression_3_2.sh
cgroup_regression_5_1.sh
cgroup_regression_5_2.sh
cgroup_regression_6_1.sh
cgroup_regression_6_2.sh
cgroup_regression_fork_processes
clock_gettime01
clock_nanosleep01
clone02
cpuhotplug01.sh ... cpuhotplug07.sh
cpuset_base_ops_testset.sh
cpuset_exclusive_test.sh
cpuset_funcs.sh
```

LA `ltp-musl` had `42` case-level `124` timeouts before the global deadline
stopped the run. Largest families observed before truncation:

| Family / prefix | Timeout count |
| --- | ---: |
| `cpuhotplug*` | `9` |
| `cgroup*` | `8` |
| `dns*` | `4` |
| `clock*` | `3` |
| `dio*` | `3` |
| `epoll*` | `3` |
| `crash*` | `2` |
| `fork*` | `2` |

Representative `ltp-musl` budget consumers before truncation:

```text
abort01
accept01
accept4_01
cgroup_fj_proc
cgroup_regression_3_1.sh
cgroup_regression_3_2.sh
cgroup_regression_5_1.sh
cgroup_regression_5_2.sh
cgroup_regression_6_1.sh
cgroup_regression_6_2.sh
cgroup_regression_fork_processes
clock_gettime01
clock_nanosleep01
clock_nanosleep04
clone02
cpuctl_fj_cpu-hog
cpuhotplug01.sh ... cpuhotplug07.sh
dns-stress-lib.sh
dns-stress.sh
dns-stress01-rmt.sh
dns-stress02-rmt.sh
epoll-ltp
epoll_pwait01
epoll_pwait03
fork14
fork_exec_loop
```

## sdcard Evidence

The official/staged full images used for faithful replays are 4G images:

```text
/home/yxzhang/oscomp-sdcard/sdcard-rv.img
SHA256 e7fdcf94d39ed39c547d076ea37369b5356414bdcf1e0a1c72c5ba294288ce71

/home/yxzhang/oscomp-sdcard/sdcard-la.img
SHA256 d86a488d990715635b1d68802c39f49f05328fef3536c91c4d02e41cb00a12d8
```

The repository-root images are small, dirty 96M images and are not the faithful
OJ/full-run evidence:

```text
/home/yxzhang/oskernel2025-x/sdcard-rv.img
SHA256 d8222fcd13e964b23524f1d51f863d715fe04c897b859c3e4cdb993cc64fb8d0

/home/yxzhang/oskernel2025-x/sdcard-la.img
SHA256 eb663b802ccecb4bdd4564869cbc611ac8d8e32f906e28b339051a24c9a33b7e
```

Copied/fsck-inspected 4G image contents show both `/glibc` and `/musl` contain
the expected 12 `*_testcode.sh` files, including LTP. `tune2fs` showing
`needs_recovery` / not-clean state on the staged images is a filesystem state
fact, but it does not by itself explain the LA LTP zero/early-death symptom.

## Conclusion

`musl-la` should be reported as **global timeout / budget truncation**. It
entered LTP and produced `150` real returned-zero cases before the LA kernel
global deadline stopped the run.

`glibc-la` should not be reported as a local LTP crash or sdcard mismatch. The
faithful replay completed `ltp-glibc` with `226 returned 0`. If a three-way
field must be filled for the OJ symptom, classify it under **global timeout /
result aggregation** with the explicit caveat that the glibc group itself
completed locally.

Recommended next checks are outside kernel functional changes:

- Confirm the exact OJ commit, entry command, image, and timeout policy.
- Confirm whether OJ discards already printed group results when the full LA run
  ends via forced shutdown or outer timeout.
- Treat future optimization work as budget reduction: LA pre-LTP perf groups and
  LTP 124 long-tail cases are the budget consumers. This packet does not change
  those behaviors.

## Day 2 Handoff Preconditions

Use this packet as the factual precondition for the next LA-LTP optimization
branch. The next task should not restart from the older assumption that LA LTP
is a hard crash or that LTP-only replays are representative.

Required starting conditions:

- Start from `main` on a dated `wip/la-ltp-<date>` branch.
- Preserve this packet before changing kernel behavior.
- Run build and regression on `ssh yxzhang@10.102.35.44` in
  `/home/yxzhang/oskernel2025-x`, with `zhouzhouyi/os-contest:20260510`.
- For faithful full/OJ-like runs, use the staged 4G official images under
  `/home/yxzhang/oscomp-sdcard/`, mounted into Docker as read-only images. Do
  not use the repo-root 96M `sdcard-{rv,la}.img` files as faithful evidence.
- Clean build before trusted runs:
  `rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; make -B all`.
  Confirm `kernel-rv` is `RVC, soft-float ABI` and `kernel-la` is LoongArch.
- A successful Day 2 result must be measured with one-architecture, one-QEMU,
  full-profile replay and one shared budget. Targeted LTP-only runs are useful
  for diagnosis, but they are not acceptance evidence.
- Count only real LTP returned-zero lines:
  `FAIL LTP CASE ... : 0` or the guest `returned 0` summary. Ignore garbage
  output and do not weaken the harness.
- The optimization target is budget reduction / real fast failure for cases
  that already hang or fail, not fake passing. Track group timings, LTP progress,
  final case, and whether termination is by kernel global deadline or host
  timeout.
- Keep the existing gates: basic-4 non-regression, RV soft-float, no OJ
  submission by Codex, no edits to the cancel-signal-futex path.
