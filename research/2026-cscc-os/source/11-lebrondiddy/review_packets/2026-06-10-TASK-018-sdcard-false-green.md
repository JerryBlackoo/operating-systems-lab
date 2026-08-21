# TASK-018 Sdcard False Green Root Cause

Date: 2026-06-10 CST
Executor: Codex in WSL workspace

## Root Cause

TASK-017 proved only the flat root layout:

```text
/basic_testcode.sh
/basic/
/busybox
```

Available contest-like sdcard assets under the sibling synced workspace have a
different root layout:

```text
/glibc/basic_testcode.sh
/glibc/basic/
/glibc/busybox
/musl/basic_testcode.sh
/musl/basic/
/musl/busybox
```

`debugfs -R 'ls -l /'` on
`/home/lenovo/oskernel2025-x/.m0-baselines/starry-next/sdcard-riscv64.img`
showed only `lost+found`, `glibc`, and `musl` at `/`. The TASK-017 kernel
scanned only regular files at `/`, so it could not see
`/glibc/basic_testcode.sh` or `/musl/basic_testcode.sh` and printed
`No basic_testcode.sh found on root filesystem`. That matches the real OJ
symptom after commit `695ba9c`.

The contest README says the test disk is raw EXT4 with no partition table.
Diagnostic MBR and GPT images failed during EXT4 mount with error `95` rather
than reaching `No basic_testcode.sh found`, so partitioning is not the observed
mismatch. QEMU device selection is also not the mismatch: both local and OJ
commands attach the test fs as drive `x0`, and the kernel mounts block device 0
as `/`.

## Fix

- Kernel test discovery now scans root regular files plus first-level root
  directories, so it finds `basic_testcode.sh` under `/`, `/glibc`, or `/musl`.
- Test scripts now run with their containing directory as cwd. This preserves
  relative paths such as `./busybox`, `cd ./basic`, and `./run-all.sh`.
- The non-ELF script fallback now locates BusyBox in the current directory or
  parent directory before falling back to `/busybox`.
- The harness default image layout is now nested under `/glibc` and `/musl`,
  while `TEST_IMAGE_LAYOUT=flat` remains available for the old regression.
  Existing `sdcard-*.img` reuse validates the expected layout with `debugfs`.

## Verification Summary

- Official image `--network none`, forced `make -B all`: produced ELF
  `kernel-rv` and `kernel-la`.
- New nested generated layout:
  - RISC-V passed with exact `#### OS COMP TEST GROUP START basic ####` and END
    markers and all required basic case lines.
  - LoongArch passed with the same markers and all required basic case lines.
- Flat regression:
  - `TEST_IMAGE_LAYOUT=flat ... ./harness/run-rv.sh` passed.
  - `TEST_IMAGE_LAYOUT=flat ... ./harness/run-la.sh` passed.
- Full starry-next sdcard diagnostic:
  - Before this fix, the root layout explains the old `No basic_testcode` miss.
  - After this fix, RISC-V starts `/glibc/basic_testcode.sh` or
    `/musl/basic_testcode.sh` and no longer prints `No basic_testcode`, but that
    full image then hits a separate page fault before the first basic case. That
    image is evidence for the script-location mismatch, not this task's passing
    basic payload.

No real judge `sdcard-rv.img` / `sdcard-la.img` or downloaded OJ output text
files were present in this WSL checkout. The investigation used
`contest_capture/`, the task-provided OJ symptom, and available captured
contest-like sdcard assets under the sibling synchronized workspace.
