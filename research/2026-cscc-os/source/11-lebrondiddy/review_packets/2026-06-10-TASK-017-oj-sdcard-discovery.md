# TASK-017 OJ Sdcard Discovery

Date: 2026-06-10 CST
Executor: Codex in WSL workspace

## Root Cause

The previous local green harness did not exercise the judge's fixed image names.
It built a temporary rootfs image and also allowed optional extra disk attachment,
while the OJ boots with a single external virtio block device named
`sdcard-rv.img` or `sdcard-la.img` as drive `x0`.

Kernel inspection confirmed QEMU platforms are built with `DRIVER := kvirtio`;
the `kramdisk`/`kernel/mount.img` path is not selected for `riscv64-qemu` or
`loongarch64-qemu`. The kernel mounts external virtio block device index 0 as
`/`. TASK-017 therefore closes the harness false-green gap and makes the kernel
run the discovered `basic_testcode.sh` instead of bypassing it with a direct
`basic/run-all.sh` command.

## Changes

- `harness/common.sh` now creates fixed OJ-shaped `sdcard-rv.img` and
  `sdcard-la.img` files by default for `basic`.
- `harness/run-rv.sh` and `harness/run-la.sh` use those fixed names as drive
  `x0` and remove optional second-disk attachment from the green gate.
- `kernel/kernel/src/tasks/initproc.rs` executes the discovered test script via
  BusyBox, with a `/busybox` fallback.
- `.gitignore` ignores generated `sdcard-rv.img` and `sdcard-la.img`.
- Acceptance and decision docs now record the OJ-shaped command path.

## Verification

Official image, no network, forced rebuild:

```sh
rm -f kernel-rv kernel-la sdcard-rv.img sdcard-la.img
docker run --rm --network none -v "$PWD:/workspace" -w /workspace \
  zhouzhouyi/os-contest:20260510 bash -lc '
    set -euo pipefail
    export RUSTUP_TOOLCHAIN=nightly-2025-01-18 CARGO_NET_OFFLINE=true
    make all
    file kernel-rv kernel-la
    TIMEOUT_SECONDS=120 ./harness/run-rv.sh
    TIMEOUT_SECONDS=120 ./harness/run-la.sh
  '
```

Exact OJ-shaped commands, with fresh fixed-name images:

```sh
qemu-system-riscv64 -machine virt -kernel kernel-rv -m 1G -nographic -smp 1 \
  -bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -no-reboot \
  -device virtio-net-device,netdev=net -netdev user,id=net -rtc base=utc
qemu-system-loongarch64 -kernel kernel-la -m 1G -nographic -smp 1 \
  -drive file=sdcard-la.img,if=none,format=raw,id=x0 \
  -device virtio-blk-pci,drive=x0 -no-reboot \
  -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -rtc base=utc
```

Results:
- `make all` produced ELF `kernel-rv` and `kernel-la`.
- `sdcard-rv.img` and `sdcard-la.img` were ext4 images populated with
  `busybox`, `basic/`, and `basic_testcode.sh`.
- RISC-V exact OJ run exited 0, found `basic_testcode.sh`, printed exact
  `#### OS COMP TEST GROUP START basic ####` and END markers, and printed all
  31 required `Testing <case> :` lines.
- LoongArch exact OJ run exited 0 with the same markers and all 31 case lines.
- Neither output contained `No basic_testcode.sh found on root filesystem`,
  `Assert Fatal`, `unknown command`, `could not be run`, `FAIL`, or panic
  signals.

## Publication

Do not submit to the judge from TASK-017. Push the local TASK-017 commit to
GitLab, then rerun/submission should occur only under an explicit TASK-012
packet or user instruction.
