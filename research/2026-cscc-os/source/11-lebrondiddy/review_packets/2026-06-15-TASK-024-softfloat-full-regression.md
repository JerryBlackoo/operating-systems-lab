# TASK-024 soft-float full regression review

Date: 2026-06-15 CST.

Candidate:
- Branch: `wip/TASK-024-softfloat`
- Commit: `25b437721703d74566befb6a5ed23ca69ff1e688`
- Kernel tree: `a29c4ef240eef2946d33147b02daf9b643d1b0a0`
- Remote: `lenovo@100.92.143.96:/home/lenovo/oskernel2025-x`

Pre-run ABI check:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
```

Basic-4 gate:
- `.hermes/mygate-rv-long.log`: `PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found`, `RVLONG_RC=0`
- `.hermes/mygate-la.log`: `PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found`
- `/tmp/task024-basic4-official-rerun-codex.log`: both RISC-V and LoongArch basic glibc/musl passed.

Full regression run:
- Script: `/tmp/run-full-regression.sh`
- Images: `/home/lenovo/Downloads/oscomp-sdcard/sdcard-rv.img`, `/home/lenovo/Downloads/oscomp-sdcard/sdcard-la.img`
- Layout: `TEST_IMAGE_LAYOUT=nested`
- Timeouts: `TIMEOUT_SECONDS=6600`, `STALL_TIMEOUT_SECONDS=600`
- Logs: `.hermes/full-rv.log` (47M), `.hermes/full-la.log` (72M)
- Sentinels: `/tmp/full-rv.done`, `/tmp/full-la.done`, `/tmp/full-all.done`

Harness RC:

```text
.hermes/full-rv.log:===RV_RC=5===
.hermes/full-la.log:===LA_RC=7===
```

`score.awk` output:

```text
== rv ==
busybox-glibc    pass(success)=55   fail=0    Pass!=0
busybox-musl     pass(success)=55   fail=0    Pass!=0
lua-glibc        pass(success)=9    fail=0    Pass!=0
lua-musl         pass(success)=9    fail=0    Pass!=0
libctest-musl    pass(success)=0    fail=0    Pass!=190

== la ==
busybox-glibc    pass(success)=23   fail=1    Pass!=0
busybox-musl     pass(success)=22   fail=0    Pass!=0
lua-glibc        pass(success)=9    fail=0    Pass!=0
lua-musl         pass(success)=9    fail=0    Pass!=0
libctest-musl    pass(success)=0    fail=0    Pass!=29
```

START/END audit:

```text
RV missing END: ltp-musl
LA missing END: ltp-glibc
```

LTP transcript counts:

```text
RV ltp-glibc: run=305, pass=0, fail=302, END present
RV ltp-musl : run=15,  pass=0, fail=14,  END missing
LA ltp-glibc: run=298, pass=0, fail=296, END missing
LA ltp-musl : not reached
```

Failure points:
- RV: `ltp-musl/add_ipv6addr` triggered `kernel page error: 0xffffffcd7d155000 sepc: 0xffffffc080268fb4`; harness then failed with `RISC-V output contains a failure signal`.
- LA: `ltp-glibc/dns-stress01-rmt.sh` produced no console output for 600s; harness killed QEMU with `HARNESS STALL`.

Non-regression decision:
- Basic-4: PASS.
- RV musl functional groups: busybox 55/55, lua 9/9, libctest 190 Pass; not below the current TASK-024 reference.
- LA functional groups: busybox-glibc 23 plus 1 fail, busybox-musl 22, lua 9/9, libctest 29 Pass; matches the current TASK-024 reference.
- Missing LTP END markers are recorded above and are not counted as passed.

Honest score estimate against the banked 1296.25:
- Banked `3000105` score was `1296.2`, with `basic306 busybox40 libctest219 lmbench120.7 iozone43 libcbench40.5 lua27` plus about 500 points from non-honest LA glibc LTP overflow garbage.
- The honest line is therefore about 1010 before TASK-024's new RV-glibc contribution.
- New confirmed RV-glibc contribution:
  - basic: +102, because RV glibc basic now passes.
  - busybox: +about 22, using the historical busybox 40 points for 100 passing cases as the anchor.
  - lua: +9, because the historical 27 points corresponded to the other three 9-script platform runs.
  - libcbench/lmbench: real RV-glibc numbers are printed, but the OJ-side performance formula is not available locally; even crediting a rough extra few tens of points does not close the remaining gap to 1296.25.
  - iozone-glibc, iperf-glibc, netperf-glibc, and LTP: no reliable honest additional credit from this run. Netperf glibc still reports `GLIBC_PRIVATE` library-version failures; LTP has zero `PASS LTP CASE` lines and missing END in tail groups.
- Conservative conclusion: this does not honestly exceed 1296.25. Do not write a new OJ-READY for human submission from this result.

Action:
- Merge TASK-024 to main only to preserve the real RV-glibc basic/busybox/lua gain, after verifying `main:kernel == a29c4ef240eef2946d33147b02daf9b643d1b0a0`.
- OJ remains human-only; no automatic submission.
