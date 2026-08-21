# 2026-06-18 Remote Eval Audit

Date: 2026-06-18
Prepared by: Codex

## Scope

Audit the remote evaluation environment, sdcard image sources, cropped-image
usage, and performance/concurrency rules before further kernel bugfix work.

No kernel functional code was changed. No OJ submission was made.

Primary remote evidence:

```text
/home/yxzhang/oskernel-artifacts/remote-eval-audit-20260618/summary-20260618-215955.log
/home/yxzhang/oskernel-artifacts/remote-eval-audit-20260618/audit-20260618-214519.log
/home/yxzhang/oskernel-artifacts/remote-eval-audit-20260618/debugfs-exists-20260618-214721.log
/home/yxzhang/oskernel-artifacts/remote-eval-audit-20260618/payload-layout-20260618-215018.log
```

## Short Answer

1. The staged 4G images under `/home/yxzhang/oscomp-sdcard/` are suitable as
   the local full/OJ-like baseline source, with the explicit caveat that the
   installed Docker image reports QEMU `10.0.2` while the captured contest
   README records QEMU `9.2.1`.
2. The repo-root 96M `sdcard-rv.img` and `sdcard-la.img` are basic/quick images
   only. They contain `basic_testcode.sh` plus basic payload under both libcs,
   but not the full 12 testcode scripts or LTP/perf payloads.
3. Cropped/basic-only images exist to make basic-4 gates and targeted diagnosis
   finish cleanly. They must not be used for score estimates or full/OJ-like
   evidence.
4. Full/OJ-like evidence must use one architecture per QEMU, a fresh disposable
   copy of the staged 4G image, all 12 testcode scripts retained, about a 7200s
   host budget, and a complete evidence manifest.
5. The remote server can be used for kernel bugfix work now. The evaluation
   blocker is not image availability; it is discipline around image source,
   disposable copies, and result manifests.

## Environment Manifest

Remote:

```text
ssh yxzhang@10.102.35.44
hostname: ubuntu
date: 2026-06-18T21:45:19+08:00
nproc: 80
memory: 251Gi total, 236Gi available at audit time
kernel: Linux ubuntu 5.15.0-130-generic x86_64
repo: /home/yxzhang/oskernel2025-x
remote HEAD at audit: b0fb8f295796e442ab62a48707e8f9ce92713186
remote branch at audit: wip/la-binfmt-20260618
```

Docker image:

```text
image: zhouzhouyi/os-contest:20260510
image id: sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac
created: 2026-05-10T08:46:16.065707166Z
architecture/os: amd64/linux
repo digests: []
```

Toolchain inside the Docker image:

```text
qemu-system-riscv64 --version: QEMU emulator version 10.0.2
qemu-system-loongarch64 --version: QEMU emulator version 10.0.2
rustc: rustc 1.86.0-nightly (6067b3631 2025-01-17)
cargo: cargo 1.86.0-nightly (088d49608 2025-01-10)
accelerators in both QEMU binaries: tcg only
/dev/kvm inside container: absent
```

Caveat: `contest_capture/raw/github/testsuits_README.md` records QEMU `9.2.1`
for both RISC-V and LoongArch. The current `zhouzhouyi/os-contest:20260510`
image on the server reports QEMU `10.0.2`. Treat this as an environment
difference in every full/OJ-like result manifest.

## Sdcard Image Classification

### Staged 4G Images

These are the full/OJ-like baseline source:

| Image | Size | SHA256 |
| --- | ---: | --- |
| `/home/yxzhang/oscomp-sdcard/sdcard-rv.img` | `4294967296` bytes | `e7fdcf94d39ed39c547d076ea37369b5356414bdcf1e0a1c72c5ba294288ce71` |
| `/home/yxzhang/oscomp-sdcard/sdcard-la.img` | `4294967296` bytes | `d86a488d990715635b1d68802c39f49f05328fef3536c91c4d02e41cb00a12d8` |

Both images have ext4 block count `1048576`, block size `4096`, and currently
show `Filesystem state: not clean with errors` plus `needs_recovery`. Do not run
`e2fsck` on these master staged images. If a run needs repair, copy the image
first and run `e2fsck -fy` only on the disposable copy.

Readonly `debugfs -c` inventory proves that both staged images contain all 12
testcode scripts under both `/glibc` and `/musl`:

```text
basic_testcode.sh
busybox_testcode.sh
cyclictest_testcode.sh
iozone_testcode.sh
iperf_testcode.sh
libcbench_testcode.sh
libctest_testcode.sh
lmbench_testcode.sh
ltp_testcode.sh
lua_testcode.sh
netperf_testcode.sh
unixbench_testcode.sh
```

Payload check summary for both architectures and both libcs:

- LTP payload exists as `/glibc/ltp/testcases/bin` and
  `/musl/ltp/testcases/bin`.
- Libctest payload exists in the official layout as `libctest_testcode.sh`,
  `runtest.exe`, `run-static.sh`, `run-dynamic.sh`, and `lib/`; there is not a
  separate top-level `/glibc/libctest` or `/musl/libctest` directory in these
  images.
- Busybox payload exists as `busybox` plus `busybox_cmd.txt`.
- Lmbench payload exists as `lmbench`, `lmbench_all`, and helper entries such
  as `lat_syscall`.
- Iozone payload exists as `iozone`.
- Additional full payloads observed include `libc-bench`, `iperf3`, `netperf`,
  `netserver`, and `lua`.

Conclusion: these are complete official-style full images for local full/OJ-like
evaluation.

### Repo-Root 96M Images

These are quick/basic images only:

| Image | Size | SHA256 |
| --- | ---: | --- |
| `/home/yxzhang/oskernel2025-x/sdcard-rv.img` | `100663296` bytes | `d8222fcd13e964b23524f1d51f863d715fe04c897b859c3e4cdb993cc64fb8d0` |
| `/home/yxzhang/oskernel2025-x/sdcard-la.img` | `100663296` bytes | `eb663b802ccecb4bdd4564869cbc611ac8d8e32f906e28b339051a24c9a33b7e` |

Readonly `debugfs -c` shows the repo-root images contain, under both libcs:

```text
OK basic_testcode.sh
OK basic
OK bin
OK libc.so
OK busybox
```

The same inventory shows these scripts and payloads are missing:

```text
busybox_testcode.sh
cyclictest_testcode.sh
iozone_testcode.sh
iperf_testcode.sh
libcbench_testcode.sh
libctest_testcode.sh
lmbench_testcode.sh
ltp_testcode.sh
lua_testcode.sh
netperf_testcode.sh
unixbench_testcode.sh
ltp/testcases/bin
runtest.exe
run-static.sh
run-dynamic.sh
lmbench_all
iozone
libc-bench
iperf3
netperf
netserver
lua
```

Conclusion: repo-root 96M images can be used for basic/quick smoke only. They
must not be cited as full/OJ-like, LA-LTP, score-estimate, or 12-group evidence.

## Cropped / Basic-Only Image Audit

Current usage patterns found in repo packets and remote artifacts:

- `harness/common.sh` builds or resolves root-level `sdcard-rv.img` /
  `sdcard-la.img` for automatic basic tests. It validates basic output; it does
  not make a full-score claim.
- Many batch packets use `basic-only` logs as basic-4 non-regression evidence.
  `review_packets/2026-06-17-LTP-overnight-batch.md` explicitly records the key
  rule: `EXPECTED_GROUPS` alone does not stop later groups, so disposable
  cropped images are used for basic-only gates.
- `review_packets/2026-06-18-LTP-chmod-batch.md` uses fresh 4G image copies
  pruned to basic-only and records that `e2fsck` repaired temporary copies
  before pruning.
- `review_packets/2026-06-18-OJ1389-LA-binfmt-rootcause.md` uses:
  - full OJ-like LA replay from a fresh 4G staged-image copy,
  - targeted LTP windows with `.ltp_case_limit` and selected hard links,
  - final cropped 4G images only for basic-4 gating.
- Remote artifacts under
  `/home/yxzhang/oskernel-artifacts/oj1389-la-binfmt-20260618/` include
  `basic4-cropped-*.img` and `full-la-ojlike-*.img`, matching that separation.

Allowed uses:

- Basic-4 non-regression gates.
- Targeted/window diagnosis when the packet clearly labels the run as
  cropped, pruned, case-limited, or targeted.
- Reproducing one failure cluster faster, with an explicit statement that it is
  not full/OJ-like evidence.

Red lines:

- Cropped/basic-only/pruned/case-limited images must not be used for full-score
  estimation.
- They must not be cited as proof that later groups, LTP, or full OJ behavior
  pass.
- A run with `.ltp_case_limit`, deleted testcode scripts, selected hard links,
  or fabricated `returned0` summaries is diagnostic only.

## Full / OJ-Like Standard

Use this as the durable rule for any future full/OJ-like packet:

1. Run one architecture per QEMU process. Keep `-smp 1`; do not change SMP to
   chase speed in faithful runs.
2. Start from a fresh disposable copy of the staged 4G image:
   `/home/yxzhang/oscomp-sdcard/sdcard-rv.img` or
   `/home/yxzhang/oscomp-sdcard/sdcard-la.img`.
3. Use the full image layout and retain all 12 `*_testcode.sh` scripts under
   both `/glibc` and `/musl`.
4. Use an outer host budget of about `7200s` unless a packet explicitly states
   why another budget is being tested.
5. Do not use `.ltp_case_limit`; do not delete testcode; do not crop payloads;
   do not fake or rewrite `returned0`.
6. Record per-group start and end times, group END markers, final observed case,
   real `returned0` counts, and whether termination came from normal QEMU exit,
   kernel global deadline, host timeout, or harness stall.
7. Every result must attach:
   - git commit and branch,
   - `kernel-rv` / `kernel-la` SHA256 and `file` output,
   - source sdcard SHA256 and disposable-copy path,
   - Docker image id,
   - QEMU version,
   - exact command,
   - full log path and driver log path.

Recommended driver shape:

```bash
ART=/home/yxzhang/oskernel-artifacts/<run-name>
mkdir -p "$ART"
cp --reflink=auto /home/yxzhang/oscomp-sdcard/sdcard-la.img "$ART/sdcard-la-run.img"
e2fsck -fy "$ART/sdcard-la-run.img"
docker run --rm --network none \
  -v /home/yxzhang/oskernel2025-x:/workspace \
  -v "$ART:/art" \
  -w /workspace \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=7200 STALL_TIMEOUT_SECONDS=600 \
            TEST_IMAGE=/art/sdcard-la-run.img TEST_IMAGE_LAYOUT=nested \
            SKIP_TEST_IMAGE_VALIDATE=0 \
            bash harness/run-la.sh'
```

The command above is a template. A real packet must still record the exact
command actually run and its log paths.

## Performance / Concurrency Policy

Copy policy:

- `cp --reflink=always` failed on the remote artifact filesystem:
  `Operation not supported`.
- Use `cp --reflink=auto` for disposable sdcard copies. It will use reflink if
  available later and fall back to a plain copy on the current filesystem.
- Do not mutate `/home/yxzhang/oscomp-sdcard/sdcard-*.img`.
- Do not run `e2fsck` on the master staged images. Run `e2fsck -fy` only on
  disposable copies.

QEMU acceleration:

- The Docker image exposes only `tcg` accelerators for both RISC-V and
  LoongArch.
- `/dev/kvm` is absent in the container.
- Faithful runs should therefore assume TCG-only timing and keep `-smp 1`.

Concurrency:

- Targeted/window diagnostic runs: at most 3-4 concurrent QEMU runs, each with
  its own independent sdcard copy and artifact directory.
- Full/OJ-like runs: at most two concurrent QEMU runs total, normally one RV and
  one LA.
- LA timing/root-cause runs: run alone when timing attribution matters, because
  concurrent TCG runs can perturb wall-clock conclusions.

Build parallelism:

- The top-level build recipe is currently documented and trusted as
  `rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; make -B all`.
- The audit did not prove `make -j$(nproc)` as a trusted gate. The Makefile has
  separate RV/LA targets, but both build under the same `kernel/` tree and share
  generated artifacts.
- Recommendation: do not change the default build gate yet. If parallel builds
  are explored, treat them as an optimization experiment and still verify the
  produced kernels with `file`, `readelf`, and basic-4 before trusting results.

## Decision

There is no hard evaluation-system blocker to starting the next kernel bugfix
branch. The server has the contest Docker image, full staged sdcard images, and
enough CPU/RAM for disciplined remote regression work.

The durable caveats are:

- current QEMU is `10.0.2`, not the README's `9.2.1`;
- full/OJ-like evidence only comes from fresh disposable copies of the staged
  4G images;
- repo-root 96M and cropped images are quick/diagnostic only;
- timing-sensitive LA diagnosis should run alone under TCG.
