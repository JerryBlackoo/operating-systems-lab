# 2026-06-18 OJ1389 LA binfmt root-cause packet

Date: 2026-06-18
Branch: `wip/la-binfmt-20260618`
Base: `main/a0fff71`

## Scope

This packet tracks the OJ 1389 LoongArch `glibc` LTP early-death wall around
`binfmt_misc01.sh`. The goal is to prove whether `binfmt_misc01.sh` itself
hangs or pollutes kernel state under a targeted replay, then either fix the root
cause or document a blocker if the local faithful environment cannot reproduce
the OJ symptom.

No OJ submission is made by Codex.

## OJ Facts To Preserve

Source: user-provided OJ summary for `main/a0fff71`.

- Overall OJ result: Accepted, total `1389.1431752924545`.
- `ltp-glibc`: `glibc-rv=1237`, `glibc-la=154`.
- `ltp-musl`: `musl-rv=1199`, `musl-la=0`.
- `glibc-la` last numeric case: `bind_noport01.sh`.
- `glibc-la` first dash case: `binfmt_misc01.sh`; every later `glibc-la`
  LTP entry is `-`.
- `musl-la` first dash case: `abort01`.
- LoongArch console output stopped near `RUN LTP CASE binfmt_misc01.sh`.
- LA pre-LTP groups took about `1754s`, so the OJ cutoff is not explained by
  pre-LTP alone consuming the full two-hour budget.

## Environment

- Runner: `ssh yxzhang@10.102.35.44`.
- Repo: `/home/yxzhang/oskernel2025-x`.
- Docker image: `zhouzhouyi/os-contest:20260510`.
- Official staged images:
  - `/home/yxzhang/oscomp-sdcard/sdcard-rv.img`
  - `/home/yxzhang/oscomp-sdcard/sdcard-la.img`
- Artifact directory:
  `/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/`.

## Build Evidence

Clean build command:

```bash
rm -rf kernel/target kernel-rv kernel-la kernel/mount.img
make -B all
file kernel-rv kernel-la
readelf -h kernel-rv | grep -i flags
```

Log:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/clean-build-20260618-184727.log
```

Observed:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

## Targeted Replay Plan

Use a disposable copy of the official 4G LA sdcard. Keep only the `glibc`
`ltp_testcode.sh` entry point and restrict `/glibc/ltp/testcases/bin` to this
window:

```text
bind05
bind06
bind_noport01.sh
binfmt_misc01.sh
binfmt_misc02.sh
block_dev
brk01
```

Required observations:

- Whether a single `binfmt_misc01.sh` run exits, times out, stalls without
  output, triggers the per-task watchdog, or prevents the `ltp-glibc` END marker.
- Whether the window run continues to `binfmt_misc02.sh` and `brk01`.
- Whether any panic, page fault, page error, or harness stall appears.

## Reproduction Commands

All commands below run on `yxzhang@10.102.35.44` in
`/home/yxzhang/oskernel2025-x`, with artifacts under
`/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618`.

Clean build:

```bash
docker run --rm --network none -e RUSTUP_TOOLCHAIN=nightly-2025-01-18 \
  -v /home/yxzhang/oskernel2025-x:/workspace -w /workspace \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; \
            make -B all; file kernel-rv kernel-la; readelf -h kernel-rv | grep -i flags'
```

Target-image preparation pattern for the valid single and window replays:

```bash
ART=/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618
cp /home/yxzhang/oscomp-sdcard/sdcard-la.img "$ART/la-binfmt-single-linked.img"
cp /home/yxzhang/oscomp-sdcard/sdcard-la.img "$ART/la-binfmt-window-linked.img"
e2fsck -fy "$ART/la-binfmt-single-linked.img"
e2fsck -fy "$ART/la-binfmt-window-linked.img"

printf '1\n' > "$ART/limit1.txt"
cat > "$ART/link-single.cmd" <<'CMDS'
ln /glibc/ltp/testcases/bin/binfmt_misc01.sh /glibc/ltp/testcases/bin/000_binfmt_misc01.sh
write /art/limit1.txt /glibc/.ltp_case_limit
rm /musl/ltp_testcode.sh
rm /glibc/basic_testcode.sh
rm /glibc/busybox_testcode.sh
rm /glibc/cyclictest_testcode.sh
rm /glibc/iozone_testcode.sh
rm /glibc/iperf_testcode.sh
rm /glibc/libcbench_testcode.sh
rm /glibc/libctest_testcode.sh
rm /glibc/lmbench_testcode.sh
rm /glibc/lua_testcode.sh
rm /glibc/netperf_testcode.sh
rm /glibc/unixbench_testcode.sh
rm /musl/basic_testcode.sh
rm /musl/busybox_testcode.sh
rm /musl/cyclictest_testcode.sh
rm /musl/iozone_testcode.sh
rm /musl/iperf_testcode.sh
rm /musl/libcbench_testcode.sh
rm /musl/libctest_testcode.sh
rm /musl/lmbench_testcode.sh
rm /musl/lua_testcode.sh
rm /musl/netperf_testcode.sh
rm /musl/unixbench_testcode.sh
CMDS
debugfs -w -f "$ART/link-single.cmd" "$ART/la-binfmt-single-linked.img"

printf '7\n' > "$ART/limit7.txt"
cat > "$ART/link-window.cmd" <<'CMDS'
ln /glibc/ltp/testcases/bin/bind05 /glibc/ltp/testcases/bin/000_bind05
ln /glibc/ltp/testcases/bin/bind06 /glibc/ltp/testcases/bin/001_bind06
ln /glibc/ltp/testcases/bin/bind_noport01.sh /glibc/ltp/testcases/bin/002_bind_noport01.sh
ln /glibc/ltp/testcases/bin/binfmt_misc01.sh /glibc/ltp/testcases/bin/003_binfmt_misc01.sh
ln /glibc/ltp/testcases/bin/binfmt_misc02.sh /glibc/ltp/testcases/bin/004_binfmt_misc02.sh
ln /glibc/ltp/testcases/bin/block_dev /glibc/ltp/testcases/bin/005_block_dev
ln /glibc/ltp/testcases/bin/brk01 /glibc/ltp/testcases/bin/006_brk01
write /art/limit7.txt /glibc/.ltp_case_limit
rm /musl/ltp_testcode.sh
rm /glibc/basic_testcode.sh
rm /glibc/busybox_testcode.sh
rm /glibc/cyclictest_testcode.sh
rm /glibc/iozone_testcode.sh
rm /glibc/iperf_testcode.sh
rm /glibc/libcbench_testcode.sh
rm /glibc/libctest_testcode.sh
rm /glibc/lmbench_testcode.sh
rm /glibc/lua_testcode.sh
rm /glibc/netperf_testcode.sh
rm /glibc/unixbench_testcode.sh
rm /musl/basic_testcode.sh
rm /musl/busybox_testcode.sh
rm /musl/cyclictest_testcode.sh
rm /musl/iozone_testcode.sh
rm /musl/iperf_testcode.sh
rm /musl/libcbench_testcode.sh
rm /musl/libctest_testcode.sh
rm /musl/lmbench_testcode.sh
rm /musl/lua_testcode.sh
rm /musl/netperf_testcode.sh
rm /musl/unixbench_testcode.sh
CMDS
debugfs -w -f "$ART/link-window.cmd" "$ART/la-binfmt-window-linked.img"
```

Valid targeted runs:

```bash
docker run --rm -v /home/yxzhang/oskernel2025-x:/code -v "$ART:/art" -w /code \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=600 STALL_TIMEOUT_SECONDS=300 \
            TEST_IMAGE=/art/la-binfmt-single-linked.img SKIP_TEST_IMAGE_VALIDATE=1 \
            EXPECTED_GROUPS=ltp-glibc bash harness/run-la.sh'

docker run --rm -v /home/yxzhang/oskernel2025-x:/code -v "$ART:/art" -w /code \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=900 STALL_TIMEOUT_SECONDS=300 \
            TEST_IMAGE=/art/la-binfmt-window-linked.img SKIP_TEST_IMAGE_VALIDATE=1 \
            EXPECTED_GROUPS=ltp-glibc bash harness/run-la.sh'
```

Full OJ-like LA replay:

```bash
ART=/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618
cp /home/yxzhang/oscomp-sdcard/sdcard-la.img "$ART/full-la-ojlike-20260618-190411.img"
docker run --rm -v /home/yxzhang/oskernel2025-x:/code -v "$ART:/art" -w /code \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=7200 STALL_TIMEOUT_SECONDS=600 \
            TEST_IMAGE=/art/full-la-ojlike-20260618-190411.img \
            SKIP_TEST_IMAGE_VALIDATE=1 \
            EXPECTED_GROUPS=basic-glibc,basic-musl,busybox-glibc,busybox-musl,lua-glibc,lua-musl,libctest-glibc,libctest-musl,libcbench-glibc,libcbench-musl,iozone-glibc,iozone-musl,lmbench-glibc,lmbench-musl,iperf-glibc,iperf-musl,netperf-glibc,netperf-musl,ltp-glibc,ltp-musl \
            bash harness/run-la.sh'
```

Final cropped-image basic-4 gate:

```bash
ART=/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618
docker run --rm -v /home/yxzhang/oskernel2025-x:/code -v "$ART:/art" -w /code \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=240 STALL_TIMEOUT_SECONDS=120 \
            TEST_IMAGE=/art/basic4-cropped-rv-20260618-210504.img \
            SKIP_TEST_IMAGE_VALIDATE=1 \
            EXPECTED_GROUPS=basic-glibc,basic-musl bash harness/run-rv.sh; \
            TIMEOUT_SECONDS=240 STALL_TIMEOUT_SECONDS=120 \
            TEST_IMAGE=/art/basic4-cropped-la-20260618-210504.img \
            SKIP_TEST_IMAGE_VALIDATE=1 \
            EXPECTED_GROUPS=basic-glibc,basic-musl bash harness/run-la.sh'
```

## Targeted Replay Results

### Invalid First Attempt

The first disposable image physically removed every non-target regular file from
`/glibc/ltp/testcases/bin`. That invalidated the case because
`binfmt_misc01.sh` sources helper files from the same directory:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/target-single-binfmt_misc01-20260618-185813.log
RUN LTP CASE binfmt_misc01.sh
sh: binfmt_misc_lib.sh: No such file or directory
sh: can't execute 'tst_run': No such file or directory
FAIL LTP CASE binfmt_misc01.sh : 127
```

This run is recorded only to explain why it is not used as kernel evidence.

### Valid Single-Case Replay

The valid single-case replay preserved the full LTP tree, added
`000_binfmt_misc01.sh` as a hard link to the real case, set
`/glibc/.ltp_case_limit = 1`, and kept only `/glibc/ltp_testcode.sh` as the test
entry point.

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/prep-linked-target-images-20260618-190029.log
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/target-single-linked-binfmt_misc01-20260618-190113.log
```

Observed:

```text
RUN LTP CASE 000_binfmt_misc01.sh
000_binfmt_misc01 1 TINFO: Running: 000_binfmt_misc01.sh
sh: can't execute 'id': No such file or directory
000_binfmt_misc01 1 TCONF: Must be super/root for this test!
FAIL LTP CASE 000_binfmt_misc01.sh : 32
initproc ltp: attempted 1 of 2841 cases, 0 returned 0
#### OS COMP TEST GROUP END ltp-glibc ####
initproc watchdog: group ltp-glibc finished in 4s (truncated)
```

No no-output stall, panic, page fault, page error, or harness stall occurred.

### Valid Window Replay

The window replay used numbered hard links plus `/glibc/.ltp_case_limit = 7`,
preserving all LTP helper files. It ran:

```text
000_bind05
001_bind06
002_bind_noport01.sh
003_binfmt_misc01.sh
004_binfmt_misc02.sh
005_block_dev
006_brk01
```

Log:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/target-window-linked-binfmt-20260618-190202.log
```

Observed:

```text
RUN LTP CASE 000_bind05
FAIL LTP CASE 000_bind05 : 124
RUN LTP CASE 001_bind06
FAIL LTP CASE 001_bind06 : 32
RUN LTP CASE 002_bind_noport01.sh
FAIL LTP CASE 002_bind_noport01.sh : 32
RUN LTP CASE 003_binfmt_misc01.sh
FAIL LTP CASE 003_binfmt_misc01.sh : 32
RUN LTP CASE 004_binfmt_misc02.sh
FAIL LTP CASE 004_binfmt_misc02.sh : 32
RUN LTP CASE 005_block_dev
FAIL LTP CASE 005_block_dev : 32
RUN LTP CASE 006_brk01
FAIL LTP CASE 006_brk01 : 0
initproc ltp: attempted 7 of 2847 cases, 1 returned 0
#### OS COMP TEST GROUP END ltp-glibc ####
initproc watchdog: group ltp-glibc finished in 28s (truncated)
```

The window crossed `binfmt_misc01.sh`, continued to `binfmt_misc02.sh`,
`block_dev`, and `brk01`, and produced the group END marker. `bind05` hit the
normal per-case watchdog; the group recovered and continued.

## Full OJ-Like Replay

Because targeted replay did not reproduce the OJ cutoff, a full LA replay was
run with a single QEMU, all groups, a fresh copy of the official staged 4G LA
image, `TIMEOUT_SECONDS=7200`, and `STALL_TIMEOUT_SECONDS=600`.

Logs:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/full-la-ojlike-20260618-190411.log
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/full-la-ojlike-driver-20260618-190411.log
```

Driver facts:

```text
HOST_START 2026-06-18T19:04:20+08:00
BRANCH wip/la-binfmt-20260618
HEAD a0fff71
IMAGE_STAT .../full-la-ojlike-20260618-190411.img 4294967296 ...
SHA256 d86a488d990715635b1d68802c39f49f05328fef3536c91c4d02e41cb00a12d8
HOST_END 2026-06-18T20:55:51+08:00 rc=5
```

Pre-LTP timings from guest watchdog lines:

```text
basic-glibc      7s exited
basic-musl       6s exited
busybox-glibc   11s exited
busybox-musl    11s exited
lua-glibc        9s exited
lua-musl         6s exited
libctest-glibc  skipped by profile-A
libctest-musl   40s exited
libcbench-glibc 60s timed out
libcbench-musl  600s timed out
iozone-glibc    180s timed out
iozone-musl     199s exited
lmbench-glibc   540s timed out
lmbench-musl    127s exited
iperf-glibc       4s exited
iperf-musl       32s exited
netperf-glibc     2s exited
netperf-musl     32s exited
```

`ltp-glibc` crossed the OJ cutoff and completed:

```text
RUN LTP CASE bind_noport01.sh
FAIL LTP CASE bind_noport01.sh : 32
RUN LTP CASE binfmt_misc01.sh
FAIL LTP CASE binfmt_misc01.sh : 32
RUN LTP CASE binfmt_misc02.sh
FAIL LTP CASE binfmt_misc02.sh : 32
RUN LTP CASE block_dev
FAIL LTP CASE block_dev : 32
RUN LTP CASE brk01
FAIL LTP CASE brk01 : 0
...
initproc watchdog: group ltp-glibc finished in 3467s (exited, code 0)
```

Full LTP counters:

| Group | Runs | Returned 0 | Per-case 124 timeouts | END marker |
| --- | ---: | ---: | ---: | --- |
| `ltp-glibc` | `2840` | `226` | `92` | yes |
| `ltp-musl` | `611` | `150` | `42` | no |

Terminal state:

```text
RUN LTP CASE fs_bind_rbind06.sh
fs_bind_rbind06 1 TINFO: Running: fs_bind_rbind06.sh
kernel watchdog: global deadline reached from timer interrupt; forcing shutdown
FAIL: LoongArch output contains a failure signal.
```

The only terminal bad signal was the kernel global deadline later in
`ltp-musl`; there was no panic, page fault, page error, kernel page error, or
harness no-output stall before or at the glibc binfmt region.

## Final Verification Gate

No kernel code was changed for this packet, but the standard non-regression gate
was still run before closing the investigation.

The first basic-4 attempt is invalid as verification evidence because it ran the
harness outside `zhouzhouyi/os-contest:20260510`, where QEMU was unavailable:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/basic4-cropped-20260618-210504.log
FAIL: qemu-system-riscv64 was not found. Run this harness inside zhouzhouyi/os-contest:20260510.
FAIL: qemu-system-loongarch64 was not found. Run this harness inside zhouzhouyi/os-contest:20260510.
```

The valid gate used disposable 4G image copies derived from the official staged
RV and LA images. The copies were repaired with `e2fsck`, then cropped so
`/glibc` and `/musl` each retained only `basic_testcode.sh`. This prevents the
guest from continuing into later groups while preserving the full official basic
payload.

Prep log:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/basic4-cropped-prep-20260618-210504.log
rv-glibc-testcode
basic_testcode.sh
rv-musl-testcode
basic_testcode.sh
la-glibc-testcode
basic_testcode.sh
la-musl-testcode
basic_testcode.sh
```

Docker gate logs:

```text
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/basic4-cropped-docker-20260618-210920.log
/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/basic4-cropped-docker-driver-20260618-210920.log
```

Driver facts:

```text
HOST_START 2026-06-18T21:09:20+0800
HEAD a0fff7131fb9726e4595c467fa2e7a9d9ed59797
BRANCH wip/la-binfmt-20260618
RV_IMAGE_STAT .../basic4-cropped-rv-20260618-210504.img 4294967296 ...
LA_IMAGE_STAT .../basic4-cropped-la-20260618-210504.img 4294967296 ...
HOST_END 2026-06-18T21:11:20+0800 rc=0
```

Observed:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
RV_RC=0
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
LA_RC=0
CONTAINER_END 2026-06-18T13:11:20+0000
```

Sanity counters from the valid run:

```text
#### OS COMP TEST GROUP START basic-glibc #### 2
#### OS COMP TEST GROUP END basic-glibc #### 2
#### OS COMP TEST GROUP START basic-musl #### 2
#### OS COMP TEST GROUP END basic-musl #### 2
PASS: RISC-V 1
PASS: LoongArch 1
RUN LTP 0
testcase busybox 0
libctest 0
kernel watchdog 0
panic 0
page fault 0
HARNESS STALL 0
FAIL: 0
```

## Conclusion / Blocker

This task escalates as a blocker rather than a kernel fix.

The OJ symptom says `glibc-la` stops at `binfmt_misc01.sh`, but the local
evidence on the requested remote server, image, branch, and Docker image shows:

- Single `binfmt_misc01.sh` returns quickly with code `32`.
- The requested window crosses `binfmt_misc01.sh`, reaches `binfmt_misc02.sh`,
  `block_dev`, and `brk01`, and prints the `ltp-glibc` END marker.
- The full OJ-like LA run crosses `binfmt_misc01.sh`, completes all `2840`
  `ltp-glibc` cases with `226` real returned-zero cases, and only terminates
  later when the global deadline fires during `ltp-musl/fs_bind_rbind06.sh`.
- The final cropped-image basic-4 gate passes for RV and LA in the required
  Docker image, and it does not accidentally run later groups.

No root-cause-backed code change was made. The remaining plausible causes are
outside this local kernel binfmt path:

- OJ image or testcase ordering differs from `/home/yxzhang/oscomp-sdcard/sdcard-la.img`.
- OJ entry command or timeout policy differs from the local single-QEMU full
  replay.
- OJ result aggregation/output capture discards or loses already printed
  `glibc-la` LTP output after the later LA global shutdown.

Recommended next step before kernel work resumes: obtain or verify the exact OJ
raw transcript/image/entry command for `a0fff71`, especially around the lines
from `bind_noport01.sh` through `brk01`, and confirm whether the judge
aggregator keeps `ltp-glibc` results when the later `ltp-musl` group hits the
global deadline.
