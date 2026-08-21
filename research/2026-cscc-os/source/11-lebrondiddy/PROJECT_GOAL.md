# Project Goal

Build an OS kernel contest workspace that can be safely handed between Claude,
Hermes/DeepSeek, and Codex without losing state, wasting model quota, or
violating contest rules.

The final technical goal is to produce an OS kernel submission for the 2026
OS kernel implementation track before the 2026-06-30 initial deadline that:

- Builds through `make all`.
- Produces the required `kernel-rv` and, later, `kernel-la` ELF outputs.
- Runs in the official contest Docker/QEMU environment.
- Mounts the contest EXT4 test image, discovers test scripts, emits the required
  `#### OS COMP TEST GROUP START/END <name> ####` markers, and exits cleanly.
- Passes an expanding local regression harness before each accepted task is
  committed.
- Maintains an honest development history with AI usage disclosure, baseline
  attribution, and meaningful commit messages.

## Current Strategy

The project follows `STRATEGY.md`: fork a dual-architecture baseline, use
ROI-ordered incremental scoring, keep a submit-ready commit available at all
times, and verify inside the official contest environment.

Claude is responsible for macro strategy, baseline selection, high-risk
architecture choices, and phase reviews. If Claude quota is exhausted, local
GPT-5.5 may temporarily cover this role according to `MODEL_ROUTING.md`.

Hermes/DeepSeek is responsible for task dispatch, mechanical validation through
the harness, state updates, logs, context compression, and batching updates for
Claude.

Codex is responsible for executing one task at a time: editing files, running
the required checks, reporting results, and escalating when the task crosses a
rule boundary.

## Strategic Decisions

Current strategic direction:

- Fork, not from scratch.
- Primary baseline family: Starry / starry-next, pending M0 real-data lock.
- Primary language: Rust.
- Architecture order: RISC-V first, then LoongArch aligned per test group.

See `DECISION_LOG.md` for durable decisions and `STRATEGY.md` for the full
campaign plan.
