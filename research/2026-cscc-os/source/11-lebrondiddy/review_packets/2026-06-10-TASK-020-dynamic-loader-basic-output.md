# TASK-020 Dynamic Loader Basic Output Fix

Date: 2026-06-10

## Symptom

The OJ run for judged source `dc9f1cb718a4c6a493d2edceb5d62971e8c385e9`
printed `basic-glibc` and `basic-musl` START/END markers and every
`Testing <case> :` prologue on both RISC-V and LoongArch, but OJ recorded
`pass=0` for every basic case. The downloaded raw output had no panic and no
case result text such as `========== START test_* ==========`, `success`, or
`Hello operating system contest`.

## Root Cause

The basic case binaries in the closest captured judge-like sdcard are dynamic
ELFs. Their PT_INTERP entries request Linux-style interpreter paths:

- RISC-V glibc: `/lib/ld-linux-riscv64-lp64d.so.1`
- RISC-V musl: `/lib/ld-musl-riscv64.so.1`
- LoongArch glibc: `/lib64/ld-linux-loongarch-lp64d.so.1`
- LoongArch musl: `/lib64/ld-musl-loongarch-lp64d.so.1`

The sdcard does not provide `/lib` or `/lib64`. It stores the loaders under the
test group directories instead:

- `/glibc/lib/ld-linux-riscv64-lp64d.so.1`, `/glibc/lib/libc.so.6`
- `/musl/lib/libc.so`
- `/glibc/lib/ld-linux-loongarch-lp64d.so.1`, `/glibc/lib/libc.so.6`
- `/musl/lib/libc.so`

Before this fix, `exec_with_process` detected PT_INTERP but ignored the
requested interpreter path and retried the program through `libc.so` in the
current working directory. For captured/OJ-like basic runs, cwd is
`/glibc/basic` or `/musl/basic`, where no `libc.so` exists. The shell therefore
printed each `Testing <case> :` line, each dynamic case failed before its own
program output, and BusyBox continued to the next case. That produced a
false-green local harness result but zero OJ passes.

After the initial PT_INTERP repair, captured RISC-V exposed one narrower
dynamic-loader issue: glibc `test_execve` started, but its child
`execve("test_echo", ...)` reached `ld-linux` with a bare program name. The
glibc loader treats a bare loader argument as a shared-object name, so it printed
`error while loading shared libraries` instead of running `./test_echo`. The
final fix passes `./<program>` to the loader when the original exec path is a
bare relative name.

## Evidence

- Direct captured-image reproduction before the fix:
  - `TEST_IMAGE=/tmp/sdcard-rv.img EXPECTED_GROUPS=basic-glibc,basic-musl`
    printed only START/END markers and `Testing <case> :` lines for both groups.
  - The old harness passed because it checked prologues only.
- Captured `/glibc/basic/run-all.sh` and `/musl/basic/run-all.sh` only echo
  `Testing $i :` before running `./$i`, so case success/failure evidence must
  come from each test binary after `execve`.
- Extracted captured binaries contain real result strings:
  - `strings .../write` includes `========== START`, `Hello operating system contest.`,
    `--- Assert Fatal ! ---`, and `========== END`.
  - `strings .../brk` includes `========== START`, `========== END`, and `test_brk`.
- `readelf -l` on captured binaries confirmed the PT_INTERP paths listed above.
- `debugfs -R 'ls -l /glibc/lib'` and `/musl/lib` showed the actual local loader
  placement; `/lib` and `/lib64` do not exist in the captured sdcard root.

## Fix

- `kernel/kernel/src/tasks/exec.rs` now reads PT_INTERP and resolves the
  requested dynamic interpreter through the exact path first, then local
  fallback paths such as `lib/<basename>`, `../lib/<basename>`, and musl
  `libc.so` fallbacks. For bare relative executable names, the loader receives
  `./<program>` so glibc `ld-linux` executes the program instead of treating it
  as a shared object.
- `kernel/kernel/src/tasks/stack.rs` sets `LD_LIBRARY_PATH` to include
  `./lib`, `../lib`, and `../../lib` so the group-local dynamic loaders can find
  their companion libc/libm files.
- `harness/common.sh` now requires real output inside each basic case block,
  not just `Testing <case> :` prologues, and it rejects incomplete START/END
  blocks or dynamic-loader errors while preserving panic/page-fault checks.
- `kernel/kernel/src/shutdown.rs` avoids a normal LoongArch completion false
  panic by writing the QEMU GED shutdown byte and idling until QEMU exits.

## Verification Summary

- Official image, `--network none`, `make -B all`: pass, produced ELF
  `kernel-rv` and `kernel-la`.
- Captured judge-like RISC-V run: pass. Both `basic-glibc` and `basic-musl`
  printed per-case `========== START test_* ==========` output and success
  text through `yield`, including `execve success.` for glibc and musl.
- Captured judge-like LoongArch run: pass with the same per-case evidence.
- Existing `TIMEOUT_SECONDS=120 ./harness/run-rv.sh`: pass.
- Existing `TIMEOUT_SECONDS=120 ./harness/run-la.sh`: pass.
- Synthetic prologue-only output: harness fails with exit 5.
- Synthetic dynamic-loader error output: harness fails with exit 5.
- Synthetic `kernel page error` output: harness fails with exit 5.

No judge submission was performed.
