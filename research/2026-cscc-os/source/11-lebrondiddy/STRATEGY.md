# Strategy

Last updated: 2026-06-08

## North Star

Fork a baseline kernel that already boots and has real test coverage, then use
ROI-ordered incremental scoring. Keep a shippable, scored version available at
all times.

The contest deadline is 2026-06-30. From 2026-06-08, that leaves about three
weeks. Building a dual-architecture kernel from scratch is not realistic. The
project strategy is therefore:

- Fork, do not start from scratch.
- Prefer a baseline with both RISC-V and LoongArch support.
- First get a real RISC-V loop green, then align LoongArch per test group.
- Prioritize high-ROI functional groups before performance and long-tail groups.
- Commit and push every accepted increment.
- Never let the repository lose its last-known-good submit-ready state.

## First Constraints

| Constraint | Meaning | Strategic impact |
|---|---|---|
| Time | Initial contest deadline is 2026-06-30 | ROI first; no perfectionism |
| Dual architecture | Must produce `kernel-rv` and `kernel-la` | Baseline must support RISC-V and LoongArch |
| Incremental scoring | Tests can be skipped; unrun tests do not score | Treat the contest as a scoring game |
| Reproducibility | Official image is `zhouzhouyi/os-contest:20260510` | Mac is never ground truth |
| Compliance | Baseline, AI usage, and commit history matter | Attribution and logs are first-class work |

## Target Layers

### Floor

Required before any aggressive expansion:

- `basic` green or nearly green.
- Most `busybox` commands working.
- Both `kernel-rv` and `kernel-la` boot and cleanly exit.
- At least one complete functional score exists.
- Compliance commits and AI/baseline attribution are in place.

### Main Target

The main contest push:

- `basic` full pass.
- `busybox` high pass rate.
- `libctest` static path.
- `lua`.
- `libcbench`.
- Useful scores from `lmbench`, `unixbench`, and `iozone`.
- Opportunistic LTP pass count.

### Stretch

Only after the floor and main target are stable:

- `iperf` / `netperf`, if the baseline already has usable TCP/IP support.
- `libctest` dynamic linking.
- `cyclictest` real-time/scheduler tuning.

## Test ROI Order

| Priority | Group | Main dependency | Strategy |
|---|---|---|---|
| P0 | `basic` | syscall/process/fs base | Fill gaps and aim to eat all points |
| P0 | `busybox` | basic + fs + fork/exec | High ROI; follow basic immediately |
| P1 | `libctest` static | musl static support | Take static first; dynamic later |
| P1 | `lua` | file I/O + libc surface | Stable functional target |
| P1 | `libcbench` | libc/syscall quality | Stable functional/perf target |
| P2 | `lmbench` | latency/bandwidth quality | Performance scoring core |
| P2 | `unixbench` | fork/exec/pipe/floating point/fs | Good composite target |
| P2 | `iozone` | filesystem throughput | Depends on FS quality |
| P3 | `ltp` | broad Linux compatibility | Long-tail points; pass what passes |
| P3 | `cyclictest` | scheduler/timer precision | Hard; defer unless cheap |
| P4 | `iperf` / `netperf` | TCP/IP stack | Only attack if baseline makes it cheap |

Implementation order is P0 -> P1 -> P2 -> P3 -> P4. For each group, get the
RISC-V loop green first, then align LoongArch before moving far ahead.

## Baseline Direction

Claude's current strategic decision is to fork the Starry/starry-next family:

- Primary target: `oscomp/starry-next`.
- Active successor candidate: `Starry-OS/StarryOS`.
- Backup: `Byte-OS/ByteOS`.

The exact fork source and commit are not locked until M0 produces real data in
the official image. The durable decision and attribution details live in
`DECISION_LOG.md`.

## Milestones

### M0: Baseline Diligence

Timebox: about 2 days.

Goal:
Clone 2-3 candidates and measure real RISC-V + LoongArch build/boot/test status
inside `zhouzhouyi/os-contest:20260510`.

Exit:

- Chosen baseline repository.
- Exact upstream commit hash.
- License recorded.
- Out-of-box pass/fail evidence recorded for RISC-V and LoongArch.
- First implementation task queue generated from real results.

### M1: Ground Loop

Timebox: about 2 days.

Goal:
Git + Docker + harness + baseline boot loop. Baseline boots in contest QEMU,
prints markers, shuts down cleanly, and has a compliant baseline attribution
commit.

### M2: Functional RISC-V

Timebox: about 6 days.

Goal:
On RISC-V, drive P0/P1: `basic`, `busybox`, `libctest` static, `lua`,
`libcbench`.

### M3: LoongArch Alignment + Performance Start

Timebox: about 6 days.

Goal:
Align M2 groups on LoongArch while starting `lmbench`, `unixbench`, and
`iozone`.

### M4: Stretch + Freeze

Timebox: about 4 days.

Goal:
Pick up LTP long-tail points, performance tuning, possible network tests, final
output/clean-shutdown checks, and compliance review.

## Risks

| Risk | Countermeasure |
|---|---|
| Wrong baseline | M0 parallel real testing; force decision after timebox |
| LoongArch lag | LA is a baseline veto; align per group |
| Time runs out | ROI queue; always keep shippable commit |
| Agent changes rot baseline | cumulative regression + commit every green |
| Compliance failure | baseline/AI attribution from first commit |
| Local/official mismatch | official Docker image is the only ground truth |

## Agent Mapping

- Claude / Claude Code / temporary GPT-5.5 substitute:
  baseline choice, macro plan, phase review, architecture decisions.
- Hermes / DeepSeek:
  expand this strategy into `TASK_QUEUE.md`, dispatch tasks, run harness,
  update logs, prepare review packets.
- Codex:
  execute one bounded task, run checks, commit/push after acceptance, escalate
  instead of changing strategy.

