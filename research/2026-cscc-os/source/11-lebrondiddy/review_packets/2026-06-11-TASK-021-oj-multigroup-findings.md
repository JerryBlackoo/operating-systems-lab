# OJ findings after TASK-021 unlock (judged commit 5657cca)

Date: 2026-06-11. Source: user-pasted live OJ console stream (RV + LA
interleaved). Judged source: GitLab `main` at
`5657cca282f2394c0dd0004afa9700213f4898fa` (TASK-021 unlock + watchdogs).
The run froze mid-`ltp-glibc` on LoongArch and stopped producing output;
expected to burn the judge's 7200s QEMU budget before scoring.

## What is PROVEN WORKING (do not regress)

- Group unlock + ordering + per-group watchdog lines all appear:
  `initproc watchdog: group <name> finished in Ns (exited|timed-out)`.
- busybox: long success streams on both arches; LA busybox-musl finished in
  37s, RV busybox-musl in 19s (`testcase busybox <cmd> success`).
- lua: ALL FOUR platform-runs fully green — 9/9 `testcase lua <s>.lua
  success` on rv-glibc, rv-musl, la-glibc, la-musl (4-6s each).
- libctest musl static: long `Pass!` streams on both arches (argv, basename,
  clocale_mbfuncs, clock_gettime, dirname, env, fdopen, fnmatch, fscanf,
  fwscanf, iconv_open, inet_pton, mbc, memstream ... until
  pthread_cancel_points).
- libcbench musl: 6 b_malloc_* results printed before hanging on
  b_malloc_thread_stress.
- iozone musl: first test (`-a -r 1k -s 4m`) printed a complete results
  table; second (`-t 4` throughput) hung.
- lmbench musl: 9 latency lines printed (Simple syscall 3.6us ... Signal
  handler overhead 95.3us) before the next item hung.
- iperf musl: client CONNECTED over loopback —
  `[ 6] local 10.0.2.15 port 20000 connected to 127.0.0.1 port 5001`.
- netperf musl: `Starting netserver with host '127.0.0.1' port '12865'`.
- cyclictest glibc: script ran start-to-end in 1s (all 4 subtests fail fast).

## Problem classes (P0 first)

### P0-A. Machine freeze in ltp-glibc; watchdog AND global deadline dead
- LA entered `#### OS COMP TEST GROUP START ltp-glibc ####`, printed many
  `RUN LTP CASE` / `FAIL LTP CASE  : 139` pairs, some LTP lib noise, then a
  garbage Summary (`passed 1396789332 / failed 1179535700 / broken
  1313817429`) and THEN NOTHING — no watchdog kill line, no global-deadline
  shutdown, no further groups, no poweroff.
- TASK-021 set an 840s global deadline. LA reached ltp-glibc around the
  500-600s cumulative mark, so the deadline should have fired mid-ltp. It
  did not. Hypothesis (must be verified): a user task drove a kernel-side
  async path into a loop that never yields/awaits, freezing the cooperative
  executor entirely — initproc (watchdog + deadline checker) never runs
  again. A watchdog that shares the executor with its victims is not a
  watchdog.

### P0-B. Budget profile is mis-sized and wastes scored groups
Measured watchdog lines from this OJ run (LA unless noted):
- libctest-glibc 30s timed-out (RV too); libctest-musl 30s timed-out —
  static suite alone needs hundreds of seconds; we killed it mid-`Pass!`
  streak.
- libcbench-musl 60s timed-out (hung on thread_stress, killed the group);
  libcbench-glibc finished in 0s with NO output at all (see P1-C).
- iozone 30s x2 timed-out; lmbench 30s x2 timed-out (killed while emitting
  real scored numbers!); cyclictest-musl 30s timed-out (hackbench);
  iperf 1s x2 timed-out (killed a working connection!); netperf 1s x2
  timed-out.
- unixbench-glibc/musl ran 30s x2 each arch — unixbench is NOT on the OJ
  score page; this is pure waste.

### P1-C. glibc dynamic-link chain systematically broken (musl healthy)
- netperf-glibc: `./netserver: ./lib/libm.so.6: version GLIBC_PRIVATE not
  found (required by ./lib/libm.so.6)` — loaded libc.so.6 does not match
  libm.so.6 (mixed library directories / wrong search order).
- libcbench-glibc: group START->END in 0s, exit code 0, zero output from
  `./libc-bench` — silent exec/loader death.
- iozone-glibc: only the script's echo lines printed; the iozone binary
  produced nothing in 30s (musl iozone printed a full table in the same
  window).
- ltp-glibc: dense `FAIL LTP CASE  : 139` (139 = 128+SIGSEGV) — dynamic LTP
  binaries segfaulting at/near load.

### P1-D. LTP scoring chain broken even where cases run
- Every ltp line prints an EMPTY case name: `RUN LTP CASE ` /
  `FAIL LTP CASE  : <ret>`. The judge keys scores off the case name, so
  even a passing case scores nothing. The script does
  `echo "RUN LTP CASE $(basename "$file")"` — the `$(basename ...)`
  command substitution yields empty. Candidate roots, in test order:
  (1) busybox sh standalone-applet re-exec needs `/proc/self/exe`, which
  procfs may not provide; (2) bare `basename` not resolvable via PATH on
  the sdcard; (3) a pipe/command-substitution defect. Fix must be
  kernel-side (procfs `/proc/self/exe`, rootfs `/bin` applet links +
  default PATH) — never edit judge scripts.
- `tst_memutils.c:93: TBROK: Expected 1 conversions got 0 FILE
  '/proc/meminfo'` — LTP parses specific meminfo fields; ours don't match.
- musl/glibc-static fwscanf: `write: No such file or directory ... failed
  to make temp file` — no usable `/tmp`.
- `tst_kconfig.c: Couldn't locate kernel config!` — minor; many LTP cases
  TBROK without /proc/config.gz (acceptable loss, note only).

### P2-E. Hang cases that must die individually, not by group kill
- libctest pthread_cancel / pthread_cancel_points: `pthread_cancel(td)
  failed: Function not implemented` then the case hangs in pthread_join
  forever (cancel never delivered). Ate the whole 30s group budget.
- libcbench b_malloc_thread_stress hang (futex/thread pressure).
- iozone `-t 4` multi-process throughput hang.
- cyclictest-musl: background `./hackbench -l 100000000 &` starved the
  shell under cooperative scheduling; group never reached STRESS subtests.

### P2-F. Small kernel-surface gaps observed
- iperf3 `-D`: `unable to become a daemon: No such device` — daemon() path
  (fork -> setsid -> open /dev/null -> dup2) returns ENODEV somewhere.
- cyclictest musl: `request to allocate mask for invalid number: Invalid
  argument` — CPU affinity/online-cpu surface (sched_getaffinity and/or
  /sys/devices/system/cpu/online).
- cyclictest glibc: `sh: can't kill pid 415: No such process` then
  `kill hackbench: fail` — `$!` pid bookkeeping vs kernel pid space.

## Measured per-group seconds from this run (for re-budgeting)
LA: busybox-musl 37; lua-glibc 6; lua-musl 4; libctest-glibc 30(T);
libctest-musl 30(T); libcbench-glibc 0; libcbench-musl 60(T);
iozone-glibc 30(T); iozone-musl 30(T); lmbench-glibc 30(T);
lmbench-musl 30(T); cyclictest-glibc 1; cyclictest-musl 30(T);
unixbench-glibc 31(T); unixbench-musl 30(T); iperf-glibc 1(T);
iperf-musl 1(T); netperf-glibc 1(T); netperf-musl 1(T); ltp-glibc FROZE.
RV: busybox-musl 19; lua-glibc 4; lua-musl 3; libctest-glibc 30(T);
libctest-musl in progress at paste cutoff. (T) = watchdog timeout kill.

## Score expectation for this submission
basic should hold 408; busybox + lua + partial libctest-musl lines were
emitted before the freeze, so some new points may register despite the
burn-out, depending on how the judge treats a timed-out run. Do NOT submit
again until TASK-023 (freeze fix) is green.
