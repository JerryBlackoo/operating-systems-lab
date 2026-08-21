# Playbook — Hard-Won Lessons (read this FIRST on any handoff)

Purpose: the non-obvious knowledge that keeps this project winning. If you are a
new session/model picking this up (Claude, GPT-5.5, Hermes, Codex), read this +
`DECISION_LOG.md` + `HANDOFF.md` before acting.

Last updated: 2026-06-08, after M0 (ByteOS baseline locked, dual-arch boot).

## Where things stand
- Phase **M0 DONE**. Baseline **LOCKED**: `Byte-OS/ByteOS @ 06171525...`
  (see DECISION-001).
- RV + LA both `justbuild` and boot to `ByteOS:/#` in the official image.
- Next phase **M1** = turn the baseline into a *submittable work*:
  `make all` -> `kernel-rv` / `kernel-la`, no `yq`, no network, emits test
  markers, scans disk, runs test scripts, shuts down.

## Environment iron rules
- Ground truth is ONLY `zhouzhouyi/os-contest:20260510` + QEMU 9.2.1. Nothing is
  "working" until it works there.
- macOS host is arm64 — NEVER the ground truth. Use it for what it is good at:
  fast network (prefetch / download / vendor).
- Division of labor that worked: **Mac downloads, WSL builds/runs.** Remote WSL
  box `ssh -p 2222 lenovo@100.92.143.96` (x86_64) runs Docker + the image.
  Passwordless ssh Mac->WSL works, so Mac can `scp` artifacts straight over.

## The Rust-kernel dependency trap (it cost us all three candidates once)
Rust kernels pull many git-pinned crates + a specific nightly. Two failure modes:
1. A pinned git dep goes private/removed -> hard death. This killed
   `starry-next`: `Starry-OS/axprocess` is 401 / "Repository not found" from both
   WSL and Mac. ALWAYS verify deps are public before committing to a baseline.
2. The repo demands a newer nightly than the eval image has -> can't bootstrap,
   and may fail at *eval time* too. This benched `StarryOS`: needs
   `nightly-2026-02-25`; image only has `nightly-2025-01-18`.

Lesson: **toolchain compatibility with the eval image is a SELECTION criterion,
not an afterthought.** ByteOS won partly because it builds under the image's own
`nightly-2025-01-18`.

## The ByteOS unblock spell (re-apply in every fresh container)
ByteOS deps are all public but WSL GitHub fetch times out. We prefetched them on
Mac and point Cargo at local copies. Inside the container:
```bash
export CARGO_NET_GIT_FETCH_WITH_CLI=true       # cargo must use system git (honors insteadOf), not libgit2
export RUSTUP_TOOLCHAIN=nightly-2025-01-18      # pin to the image's nightly; do NOT bootstrap a newer one
git config --global url."file:///work/byteos-deps/github.com/".insteadOf "https://github.com/"
export PATH="/work/<dir-with-yq>:$PATH"         # eval image has NO yq
```
- Prefetched bundle: `.m0-baselines/byteos-deps.tar.gz` (sha256 `08c2541a...`),
  5 git deps under `github.com/<org>/<repo>` with Cargo.lock-exact casing and
  `.git` suffixes: rust-fatfs, lose-net-stack, lwext4_rust (Azure-stars),
  jasonwhite/syscalls, Byte-OS/timestamp.
- Codex also added submodule `gkostka/lwext4 #58bcf89a...` — keep it.
- Build: `make PLATFORM=riscv64-qemu justbuild` / `loongarch64-qemu`.
- `run` exits **124** under `timeout` — that is the interactive shell being
  killed, NOT a failure.

## Known landmines (carry forward)
- `yq`: eval image lacks it. Our own `make all` MUST NOT depend on yq. Either
  freeze the generated config artifacts into the repo, or replace the yq step.
- Eval-time network: assume NONE. `cargo vendor` everything -> also satisfies the
  contest rule "third-party libraries shipped as source with the kernel".
- LoongArch is a second-class citizen everywhere. Align it per test group, never
  defer to the end (DECISION-002).
- ByteOS code currently lives in WSL `.m0-baselines/` (gitignored) — it is NOT in
  the contest repo yet. M1 step 1 is importing it properly with attribution.

## Submission strategy (answers "do we submit to the judge now?")
**NO, not at 94e108d.** Repo root has no `make all` -> kernel-rv/kernel-la, and
the kernel source isn't even in the contest repo. Submitting now scores 0 and
wastes a narrative commit.
- Keep pushing to GitLab continuously: the >=8-commit, 3-7-day-spacing, commented
  history is itself graded. Every accepted task = one commented commit.
- First *judge submission* should be EARLY and SMALL: the moment `make all`
  yields kernel-rv/kernel-la that boots, emits the `#### OS COMP TEST GROUP
  START/END <name> ####` markers, and passes even ONE basic point. Goal #1 is
  proving the end-to-end eval pipeline works on the judge machine — de-risk the
  pipeline before stacking features.

## ROI order for test groups (from STRATEGY)
basic -> busybox -> libctest(static) -> lua / libcbench ->
lmbench / unixbench / iozone -> LTP (long tail) ->
network (iperf / netperf, only if the net stack pays off).
ByteOS ships lose-net-stack + lwext4_rust, so the hard groups have a foundation.

## Compliance reflexes
- First baseline-import commit MUST record: ByteOS repo + exact commit
  (`06171525...`) + license + AI disclosure (model names + usage). See
  `GIT_WORKFLOW.md`.
- Every commit message: task id + AI-usage trailer.
