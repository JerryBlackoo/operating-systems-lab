# TASK-022 busybox syscall/procfs surface

Date: 2026-06-11
Executor: Hermes/Codex in WSL

## Summary

The busybox group was unlocked and measured on both architectures and both
libcs. The captured rich images emit 55 `testcase busybox ...` records per
platform-run because this `busybox_cmd.txt` includes `clear` and `printf`.
The task threshold was 45/53; final current-tree results are:

- rv-glibc: 54/55 success
- rv-musl: 54/55 success
- la-glibc: 27/55 success
- la-musl: 53/55 success

The remaining material blocker is LoongArch glibc static busybox. Focused
syscall tracing showed no remaining ENOSYS in the failing applets; repeated
`wait4(-1, ..., WNOHANG)` returns `ECHILD`, glibc also probes
`/proc/self/exe`, and several applets exit nonzero from static glibc runtime
paths even after the procfs/syscall surface fixes below.

## Per-command before/after

`before` logs: `.hermes/task022/baseline-rv.log`,
`.hermes/task022/baseline-la.log`

`after` logs: `.hermes/task022/final-rv-current.log`,
`.hermes/task022/final-la-current.log`

| # | Command | rv-glibc | rv-musl | la-glibc | la-musl |
|---:|---|---|---|---|---|
| 1 | `echo "#### independent command test"` | success->success | success->success | success->success | success->success |
| 2 | `ash -c exit` | success->success | success->success | success->success | success->success |
| 3 | `sh -c exit` | success->success | success->success | success->success | success->success |
| 4 | `basename /aaa/bbb` | success->success | success->success | success->success | success->success |
| 5 | `cal` | success->success | success->success | fail->fail | success->success |
| 6 | `clear` | success->success | success->success | success->success | success->success |
| 7 | `date` | success->success | success->success | fail->fail | success->success |
| 8 | `df` | success->success | success->success | fail->fail | success->success |
| 9 | `dirname /aaa/bbb` | success->success | success->success | fail->fail | success->success |
| 10 | `dmesg` | success->success | success->success | success->success | success->success |
| 11 | `du` | success->success | success->success | fail->fail | success->success |
| 12 | `expr 1 + 1` | success->success | success->success | fail->fail | success->success |
| 13 | `false` | success->success | success->success | success->success | success->success |
| 14 | `true` | success->success | success->success | success->success | success->success |
| 15 | `which ls` | success->success | success->success | fail->fail | fail->fail |
| 16 | `uname` | success->success | success->success | fail->fail | success->success |
| 17 | `uptime` | success->success | success->success | fail->fail | success->success |
| 18 | `printf "abcn"` | success->success | success->success | fail->fail | success->success |
| 19 | `ps` | success->success | success->success | fail->fail | success->success |
| 20 | `pwd` | success->success | success->success | fail->fail | success->success |
| 21 | `free` | success->success | success->success | fail->fail | success->success |
| 22 | `hwclock` | success->success | success->success | fail->fail | success->success |
| 23 | `sh -c 'sleep 5' & ./busybox kill $!` | fail->fail | fail->fail | fail->fail | fail->fail |
| 24 | `ls` | success->success | success->success | fail->fail | success->success |
| 25 | `sleep 1` | success->success | success->success | success->success | success->success |
| 26 | `echo "#### file opration test"` | success->success | success->success | success->success | success->success |
| 27 | `touch test.txt` | success->success | success->success | success->success | success->success |
| 28 | `echo "hello world" > test.txt` | success->success | success->success | success->success | success->success |
| 29 | `cat test.txt` | success->success | success->success | success->success | success->success |
| 30 | `cut -c 3 test.txt` | success->success | success->success | fail->fail | success->success |
| 31 | `od test.txt` | success->success | success->success | fail->fail | success->success |
| 32 | `head test.txt` | success->success | success->success | fail->fail | success->success |
| 33 | `tail test.txt` | success->success | success->success | success->success | success->success |
| 34 | `hexdump -C test.txt` | success->success | success->success | fail->fail | success->success |
| 35 | `md5sum test.txt` | success->success | success->success | fail->fail | success->success |
| 36 | `echo "ccccccc" >> test.txt` | success->success | success->success | success->success | success->success |
| 37 | `echo "bbbbbbb" >> test.txt` | success->success | success->success | success->success | success->success |
| 38 | `echo "aaaaaaa" >> test.txt` | success->success | success->success | success->success | success->success |
| 39 | `echo "2222222" >> test.txt` | success->success | success->success | success->success | success->success |
| 40 | `echo "1111111" >> test.txt` | success->success | success->success | success->success | success->success |
| 41 | `echo "bbbbbbb" >> test.txt` | success->success | success->success | success->success | success->success |
| 42 | `sort test.txt | ./busybox uniq` | success->success | success->success | fail->fail | success->success |
| 43 | `stat test.txt` | success->success | success->success | fail->fail | success->success |
| 44 | `strings test.txt` | success->success | success->success | fail->fail | success->success |
| 45 | `wc test.txt` | success->success | success->success | fail->fail | success->success |
| 46 | `[ -f test.txt ]` | success->success | success->success | success->success | success->success |
| 47 | `more test.txt` | success->success | success->success | fail->fail | success->success |
| 48 | `rm test.txt` | success->success | success->success | success->success | success->success |
| 49 | `mkdir test_dir` | success->success | success->success | success->success | success->success |
| 50 | `mv test_dir test` | success->success | success->success | success->success | success->success |
| 51 | `rmdir test` | success->success | success->success | success->success | success->success |
| 52 | `grep hello busybox_cmd.txt` | success->success | success->success | fail->fail | success->success |
| 53 | `cp busybox_cmd.txt busybox_cmd.bak` | success->success | success->success | success->success | success->success |
| 54 | `rm busybox_cmd.bak` | success->success | success->success | success->success | success->success |
| 55 | `find -name "busybox_cmd.txt"` | success->success | success->success | fail->fail | success->success |

## Kernel gaps fixed

- `statfs` layout/surface: extended `StatFS` with Linux-compatible `frsize`,
  `flags`, and spare padding, and populated these fields for ext4, ext4rs,
  fatfs, procfs, and devfs.
- Procfs content: added readable `/proc/meminfo`, `/proc/mounts`,
  `/proc/uptime`, `/proc/stat`, `/proc/cmdline`, and minimal `/proc/1`
  `cmdline`, `comm`, and `stat` content for `free`, `uptime`, `ps`, and `df`.
- `sysinfo`: writes a real Linux-shaped sysinfo struct instead of one byte.
- Time/task compatibility: accepted `set_robust_list`, returned parent pid or
  `1` from `getppid`, and accepted common coarse/raw/boottime `clock_gettime`
  ids.
- Exec environment: propagated a workdir-aware `PATH`, preserved caller envp
  through exec stack setup, resolved programs through `PATH`, and added a
  busybox applet fallback for common applet names.
- ELF loading: zero-filled segment memory past `p_filesz` up to `p_memsz`.
- `/proc/self/exe`: recorded the current exec path and answered the traced
  `readlinkat("/proc/self/exe")` probe from static glibc.

## Commands

Primary verification commands:

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'export RUSTUP_TOOLCHAIN=nightly-2025-01-18 CARGO_NET_OFFLINE=true; make -B all; file kernel-rv kernel-la'
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'TEST_IMAGE=/workspace/.m0-baselines/starry-next/sdcard-riscv64.img TEST_IMAGE_LAYOUT=nested TIMEOUT_SECONDS=900 EXPECTED_GROUPS=basic-glibc,basic-musl,busybox-glibc,busybox-musl,lua-glibc,lua-musl,libctest-glibc,libctest-musl,libcbench-glibc,libcbench-musl,iozone-glibc,iozone-musl,lmbench-glibc,lmbench-musl,cyclictest-glibc,cyclictest-musl,unixbench-glibc,unixbench-musl,iperf-glibc,iperf-musl,netperf-glibc,netperf-musl ./harness/run-rv.sh'
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'TEST_IMAGE=/workspace/.m0-baselines/starry-next/sdcard-loongarch64.img TEST_IMAGE_LAYOUT=nested TIMEOUT_SECONDS=900 EXPECTED_GROUPS=basic-glibc,basic-musl,busybox-glibc,busybox-musl,lua-glibc,lua-musl,libctest-glibc,libctest-musl,libcbench-glibc,libcbench-musl,iozone-glibc,iozone-musl,lmbench-glibc,lmbench-musl,cyclictest-glibc,cyclictest-musl,unixbench-glibc,unixbench-musl,iperf-glibc,iperf-musl,netperf-glibc,netperf-musl ./harness/run-la.sh'
```

Focused investigation:

```sh
docker run --rm --network none -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'TEST_IMAGE=/workspace/.hermes/task022/focus/la-busybox-glibc.img TEST_IMAGE_LAYOUT=nested TIMEOUT_SECONDS=180 EXPECTED_GROUPS=busybox-glibc ./harness/run-la.sh'
rg 'TRACE_SYSCALL|TRACE_EXIT' kernel
git diff --check
```

## Harness results

- Official-image `make -B all; file kernel-rv kernel-la`: passed.
- Final RISC-V rich-image multi-group run: passed harness, basic remained fully
  green, busybox/lua/libctest groups executed, no panic/page-fault markers.
- Final LoongArch rich-image multi-group run: passed harness, basic remained
  fully green, busybox/lua/libctest groups executed, no panic/page-fault
  markers.
- Focused LoongArch glibc busybox run: reproduced 27/55 success with no kernel
  panic.

## Remaining blockers

- rv-glibc, rv-musl: only line 23 fails:
  `sh -c 'sleep 5' & ./busybox kill $!`. The child exits before kill observes
  it, so busybox reports `can't kill pid ... No such process`.
- la-musl: line 15 `which ls` and line 23 fail. PATH/app fallback is enough for
  command execution but the musl `which` case still does not discover `ls` in
  this image layout.
- la-glibc: lines 5, 7, 8, 9, 11, 12, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
  30, 31, 32, 34, 35, 42, 43, 44, 45, 47, 52, and 55 fail. Focused tracing
  shows static LoongArch glibc busybox applets frequently probe
  `/proc/self/exe`, `/etc/localtime`, and use `wait4(-1, ..., WNOHANG)` paths;
  after fixing the procfs/syscall probes, these applets still return nonzero
  under the static glibc runtime. This needs a deeper LoongArch glibc process
  wait/exit or ABI investigation, not more procfs text.
