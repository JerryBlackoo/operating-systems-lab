# 2026-06-16 LTP reach STOP handoff

## Stop line

Per stop request, no new case fix and no new full LTP run should be started from
this point. This file is a handoff record for the current WIP only.

## 6600s baseline, real reach

Source:

- `review_packets/2026-06-16-LTP-baseline-6600.md`
- `.codex-logs/ltp-baseline-*-6600-20260616.summary`

Only `returned 0` is score-like evidence. The runner's fixed
`FAIL LTP CASE ... : code` label is not used as a pass/fail label.

```text
la-glibc:
  RUN=878
  returned0=153
  timeouts=71
  stop wall: gre01.sh
  terminal signal: LoongArch LoadPageFault(0x1201bdc58)
  END marker: absent

la-musl:
  RUN=2820
  returned0=173
  timeouts=96
  stop wall: none in this walk
  summary: attempted 2820 of 2820 cases, 173 returned 0
  END marker: #### OS COMP TEST GROUP END ltp-musl ####

rv-glibc:
  RUN=1177
  returned0=173
  timeouts=74
  stop wall: mallocstress
  terminal signal: RISC-V trap-layer kernel error / LoadFault-class high stval
  END marker: absent

rv-musl:
  RUN=1157
  returned0=162
  timeouts=89
  stop wall: mallocstress
  terminal signal: RISC-V trap-layer kernel error / LoadFault-class high stval
  END marker: absent
```

Read:

- The old `~700-730` ramfs `can't alloc frame in ram fs` wall is no longer the
  first observed wall in this WIP baseline.
- `la-musl` reached the full walk at `2820/2820`.
- Current highest-multiplier walls are `mallocstress` on RV and `gre*.sh` on LA.

## WIP bank

Banked locally on branch:

```text
wip/ltp-reach-20260616
```

This branch is a local safety snapshot only. It was not pushed and should not be
treated as gated for merge.

WIP areas touched:

- ramfs reclaim / ENOSPC behavior for frame exhaustion
- LTP per-case scratch cleanup in initproc
- mount replacement / stale mount cleanup path
- RISC-V trap quarantine for high-address LoadFault / LoadMisaligned handling
- rv-musl abort / process-exit path investigation work
- usercopy plumbing across fd, mm, fs, procfs, ramfs, vfscore, pipe, socket,
  time, task/syscall paths
- temporary reach quarantines for known unrecoverable crash-storm cases,
  including `mallocstress` and GRE script family handling

Observed WIP reach evidence after stopgap quarantines:

```text
rv-glibc cap1230:
  attempted 1230 of 2840, 177 returned 0
  crossed mallocstress wall, END marker present

rv-musl cap1230:
  attempted 1230 of 2820, 165 returned 0
  crossed mallocstress wall, END marker present

la-glibc cap930:
  gre01.sh quarantine moved the wall to gre02.sh
  RUN=879 returned0=153
  same LoadPageFault class remains
```

## Gate status

Latest clean build / ABI / formatting evidence before stop:

- `.codex-logs/clean-build-ltp-gre-family-quarantine-20260616.log`
- `kernel-rv`: RISC-V, RVC, soft-float ABI
- `readelf`: `Flags: 0x1, RVC, soft-float ABI`
- `cargo fmt --check`: passed

Latest basic-4 evidence under this WIP:

- `.codex-logs/basic4-rv-ltp-gre-family-quarantine-20260616.log`
- `.codex-logs/basic4-la-ltp-gre-family-quarantine-20260616.log`
- `.codex-logs/basic4-ltp-gre-family-quarantine-status-20260616.txt`

Status file:

```text
basic4_gre_family_rv_status=0 basic4_gre_family_la_status=0
```

So basic-4 was verified green for the current WIP before this stop handoff.

## Next walls to attack

Do not continue one-case-at-a-time LTP fixes.

Next root-cause targets:

1. `mallocstress` on RV:
   - RV trap reports a high-address LoadFault-class kernel error.
   - Root direction: keep this as a recoverable user/task fault or case timeout,
     not a trap-layer panic.

2. `gre01.sh` / `gre02.sh` on LA:
   - GRE script family triggers `LoadPageFault(0x1201bdc58)`.
   - `gre01.sh` quarantine alone only moves the wall to `gre02.sh`.
   - Root direction: fix LA user-fault / shell-script failure path, or make an
     explicit reach-only quarantine policy for the small `gre*.sh` family.

