# Acceptance Criteria

This project treats machine-checkable results as the source of truth.

## Global Acceptance

A task is accepted only when all applicable conditions are true:

1. The task's own acceptance criteria in `TASK_QUEUE.md` are satisfied.
2. The required harness command passes, or the task is explicitly marked blocked.
3. Previously passing regression checks still pass.
4. The diff is limited to the files allowed by the task.
5. No existing tests or checks were deleted, bypassed, weakened, or reworded to
   hide failure.
6. AI usage and decisions are recorded in `AGENT_LOG.md` and, when relevant,
   `DECISION_LOG.md`.
7. The completed task is committed with a meaningful message and trailers.

## Contest Build Acceptance

The contest build path must eventually satisfy:

- `make all` runs inside the official contest environment or an equivalent
  pinned Dockerfile.
- The build emits `kernel-rv`.
- After RISC-V is stable, the build emits `kernel-la`.
- Generated outputs are ELF kernels, not placeholder files.

## Harness Acceptance

The local harness must eventually verify:

- The official QEMU command can boot the produced kernel.
- The RISC-V harness reproduces the OJ-shaped fixed image path:
  `qemu-system-riscv64 -machine virt -kernel kernel-rv -m 1G -nographic -smp 1 -bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -no-reboot -device virtio-net-device,netdev=net -netdev user,id=net -rtc base=utc`.
- The LoongArch harness reproduces the OJ-shaped fixed image path:
  `qemu-system-loongarch64 -kernel kernel-la -m 1G -nographic -smp 1 -drive file=sdcard-la.img,if=none,format=raw,id=x0 -device virtio-blk-pci,drive=x0 -no-reboot -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -rtc base=utc`.
- Test output contains required group markers:
  `#### OS COMP TEST GROUP START <name> ####`
  and
  `#### OS COMP TEST GROUP END <name> ####`.
- QEMU exits cleanly before timeout.
- Regression groups already marked green continue to pass.

The scaffold harness is allowed to fail until a real baseline/kernel exists.
False green placeholder kernels are not acceptable.

## Test Group Acceptance

| Group | Harness evidence required | Current status |
|---|---|---|
| basic | QEMU exits 0; output contains exact `basic` or judge-style `basic-glibc`/`basic-musl` START/END markers; output contains every `Testing <case> :` line from `basic/run-all.sh` (`brk`, `chdir`, `clone`, `close`, `dup2`, `dup`, `execve`, `exit`, `fork`, `fstat`, `getcwd`, `getdents`, `getpid`, `getppid`, `gettimeofday`, `mkdir_`, `mmap`, `mount`, `munmap`, `openat`, `open`, `pipe`, `read`, `times`, `umount`, `uname`, `unlink`, `wait`, `waitpid`, `write`, `yield`); every printed case prologue is followed by a complete real test-binary START/END output block; output has no failure signal. | Green for RISC-V and LoongArch in the official image, including captured `basic-glibc`/`basic-musl` sdcard layout, as of TASK-020. |
| busybox | START/END `busybox`; each command in `busybox_cmd.txt` emits its success line and no fail line. | Pending. |
| libctest | START/END `libctest`; static and dynamic libc test scripts both run to completion without failure output. | Pending. |
| lua | START/END `lua`; each Lua script emits a success line and no fail line. | Pending. |
| libcbench | START/END `libcbench`; `libc-bench` runs to completion without failure output. | Pending. |
| lmbench | START/END `lmbench`; benchmark subcommands complete and print their result lines without failure output. | Pending. |
| unixbench | START/END `unixbench`; each UnixBench subtest prints a result line without failure output. | Pending. |
| iozone | START/END `iozone`; each iozone mode prints result output without failure output. | Pending. |
| iperf | START/END `iperf`; UDP/TCP loopback cases emit success lines and no fail line. | Pending. |
| netperf | START/END `netperf`; loopback cases emit success lines and no fail line. | Pending. |
| cyclictest | START/END `cyclictest`; no-stress and stress runs emit success lines and no fail line. | Pending. |
| ltp | START/END `ltp`; each executed LTP case reports an acceptable exit status and no failure output. | Pending. |

## Current Minimum Acceptance

Before M0 locks the exact baseline repository and commit, the workflow scaffold
is accepted if:

- Workflow documents exist.
- `review_packets/` exists.
- `harness/run-rv.sh` exists and fails clearly when no real `kernel-rv` exists.
- The remote execution strategy is documented.

## M0 Baseline Diligence Acceptance

M0 is accepted only when:

- Candidate testing happens inside `zhouzhouyi/os-contest:20260510`, or a
  documented equivalent ground-truth environment approved by the user.
- RISC-V and LoongArch build/boot/test evidence is recorded for each tested
  candidate.
- The chosen baseline repository and exact commit hash are recorded in
  `DECISION_LOG.md`.
- The license is recorded and confirmed contest-compliant.
- The first baseline attribution commit plan is written.
- The implementation task queue is updated from real pass/fail data, not memory
  or reputation.
