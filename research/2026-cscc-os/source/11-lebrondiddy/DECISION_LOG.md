# Decision Log

Record durable project decisions here. Do not use this for routine task logs.

## Decisions

### DECISION-001: Baseline and Language

Status: LOCKED 2026-06-08 after M0 official-image build/boot probes.
(Original direction was Starry/starry-next; changed to ByteOS, then confirmed
by dual-architecture official-image build/boot evidence.)

Decision (current):
Fork `Byte-OS/ByteOS` as the primary baseline. Primary language: Rust.
Native riscv64 + loongarch64 via PolyHAL cross-architecture HAL.

- Primary: `Byte-OS/ByteOS` @ `06171525001d73f8660c9fb23b11690b32cefe3d`.
  Flat-architecture componentized kernel. Ships its own net stack
  (lose-net-stack) and ext4 (lwext4_rust) — the basis for the hardest test
  groups. Exact commit is locked; record license before the first attribution
  commit.
- Fallback: `Starry-OS/StarryOS` @ `2e075acc...` (Apache-2.0, actively
  maintained). Use only if ByteOS hits a deeper wall AND its required
  `nightly-2026-02-25` toolchain can be proven available/installable inside the
  eval image `zhouzhouyi/os-contest:20260510`.
- REJECTED: `oscomp/starry-next` — depends on
  `Starry-OS/axprocess.git#f6f0d08f`, now private/removed (HTTP 401 /
  "Repository not found" from both WSL and macOS) and archived upstream. Dead
  dependency, not recoverable.

M0 Findings (2026-06-08, official image; command-level evidence in
docs/m0-baseline-results.md):
- starry-next: reaches Cargo build, dies on inaccessible private dep axprocess.
- StarryOS: stalls at `cargo axplat --version`; needs nightly-2026-02-25 absent
  from the eval image; rustup bootstrap stalls in WSL. Toolchain gap = eval risk.
- ByteOS: needs `yq` + bypass of rust-analyzer bootstrap; with
  `RUSTUP_TOOLCHAIN=nightly-2025-01-18` it reaches Cargo build, then only times
  out fetching public git deps in WSL. Shallowest, most recoverable blocker.

Why ByteOS wins on the evidence:
- Blocker is WSL network, not a dead dependency. All 5 git deps verified PUBLIC +
  reachable from macOS: rust-fatfs, lose-net-stack, lwext4_rust (Azure-stars),
  jasonwhite/syscalls, Byte-OS/timestamp.
- Builds under the eval image's own toolchain (nightly-2025-01-18 forced OK) →
  best evaluation-image compatibility. StarryOS's newer-nightly requirement is a
  real eval-time risk that ByteOS does not carry.
- Still meets the original hard requirements: native dual-arch, Rust
  (agent-friendly), public ecosystem.

Recoverability plan (this is the rest of M0 — lock baseline once it boots):
1. Unblock fast: pre-fetch the 5 git deps where the network works (macOS) → ship
   to WSL → point Cargo/git at local copies (git insteadOf / file://), OR
   configure a GitHub mirror in WSL. Confirm ByteOS builds an RV ELF + boots.
2. Make it durable + compliant: `cargo vendor` ALL deps into the repo so builds
   need no network. Also satisfies the rule that third-party libraries ship as
   source with the kernel.
3. Resolve when wiring `make all` (M1): eval image has no `yq` (our `make all`
   must not depend on it); keep the toolchain pinned to the image's nightly;
   record ByteOS license + exact commit in the first commit.

Compliance hook:
First commit must record baseline repo + exact upstream commit hash + license,
plus the AI-tooling disclosure (model names + usage). See GIT_WORKFLOW.

Lock evidence:
- Claude prefetched ByteOS git dependencies on macOS and copied
  `byteos-deps.tar.gz` to WSL; Codex added missing submodule
  `gkostka/lwext4.git#58bcf89a121b72d4fb66334f1693d3b30e4cb9c5`.
- RV official-image build:
  `make PLATFORM=riscv64-qemu justbuild` exits 0 and produces
  `target/riscv64gc-unknown-none-elf/release/kernel.bin`.
- RV official-image boot:
  `make PLATFORM=riscv64-qemu run` reaches `ByteOS:/#`; timeout exit 124 is
  expected because QEMU is left at an interactive shell.
- LA official-image build:
  `make PLATFORM=loongarch64-qemu justbuild` exits 0 and produces
  `target/loongarch64-unknown-none/release/kernel.bin`.
- LA official-image boot:
  `make PLATFORM=loongarch64-qemu run` reaches `ByteOS:/#`; timeout exit 124 is
  expected because QEMU is left at an interactive shell.
- Logs are under `/home/lenovo/oskernel2025-x/logs/m0-baseline/` and summarized
  in `docs/m0-baseline-results.md`.

### DECISION-002: Architecture Order

Status: DECIDED 2026-06-08 (by Claude) — RISC-V first, LoongArch aligned per group.

Decision:
Get the full loop (build → QEMU → markers → clean exit → pass a group) green on
RISC-V first, then port/align that same group to LoongArch before moving on.
Do not let LoongArch drift far behind: align per test group, not at the very end.

Reason:
The contest requires both `kernel-rv` and `kernel-la`, but building one real
boot/test loop first reduces surface area and avoids false progress. Because the
chosen baseline (ByteOS via PolyHAL) is natively dual-arch, per-group LA alignment stays cheap
if done continuously rather than deferred.

### DECISION-003: LoongArch Harness QEMU Device Attachment

Status: DECIDED 2026-06-09 by TASK-014 official-image probe.

Decision:
Use the official-image-supported LoongArch PCI virtio command in the harness:
`qemu-system-loongarch64 -kernel kernel-la -m 1G -nographic -smp 1 -drive file=sdcard-la.img,if=none,format=raw,id=x0 -device virtio-blk-pci,drive=x0 -no-reboot -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -rtc base=utc`.

Reason:
The contest README-shaped LoongArch command attaches `virtio-blk-pci` to
`bus=virtio-mmio-bus.N`. In the official image, `qemu-system-loongarch64`
rejects that before boot with `Bus 'virtio-mmio-bus.0' not found`.
`qemu-system-loongarch64 -device help` reports `virtio-blk-pci` and
`virtio-net-pci` as PCI devices, and the default LoongArch `virt` machine
accepts them without an explicit bus. This also matches ByteOS's previously
verified `loongarch64-qemu run` command shape.

Update from TASK-017:
The OJ run uses fixed `sdcard-la.img` as the only block device and no `hostfwd`
arguments. The harness now mirrors that shape so local green gates cover the
same rootfs discovery path the judge uses.

### DECISION-004: Basic Test Script Discovery Layout

Status: DECIDED 2026-06-10 by TASK-018 evidence.

Decision:
Discover `*_testcode.sh` files at the root of the test filesystem and one level
below root, including ABI subdirectories such as `/glibc` and `/musl`. Run each
script with its containing directory as the user task cwd.

Reason:
TASK-017 only tested a flat generated image with `/basic_testcode.sh`. Available
contest-like sdcard captures under `.m0-baselines/starry-next/` have only
`glibc/` and `musl/` at `/`, with `basic_testcode.sh`, `busybox`, and `basic/`
inside those directories. A root-only scan exactly explains the real OJ
`No basic_testcode.sh found on root filesystem` output after commit `695ba9c`.
The contest README says the judge disk is flat EXT4 with no partition table;
diagnostic MBR/GPT images failed at EXT4 mount instead of reaching the
`No basic_testcode` path, so partitioning is not the observed mismatch.

### DECISION-005: Keep Hermes as the execution rail; add a sentinel layer; move the watch to local Codex

Status: DECIDED 2026-06-12 by the user after the TASK-023 experience.

Decision:
1. Hermes is KEPT, not abandoned. It remains the execution rail:
   packaging a task card, launching detached remote `codex exec`,
   mirroring logs, recording run state, auto-result blocks. That part
   worked all week and costs zero LLM tokens (it is bash).
2. What Hermes is NOT: a scheduler, a watchdog, or a governor. Its
   observed gaps this week: no run-to-run relay (idle gaps after every
   finish), stale `running` states needing manual repair, and no
   in-flight governance (TASK-023 iterated ~15 LA rounds before a human
   imposed a closure rule).
3. Those gaps are filled by ROLES, not by replacing the tool:
   - Machines watch machines: `scripts/hermes-progress.sh` (one-shot or
     --watch) is the standard probe; nobody asks the remote Codex for
     status anymore.
   - Local Codex owns the watch, boundary reviews, and the relay per
     `.hermes/LOCAL-CODEX-SENTINEL-BRIEF.md` (cadence, signal->response
     table, rescue-commit recipe, per-task playbooks, escalation
     matrix).
   - Claude is planning/scope only; the user is interrupted only for OJ
     clicks, scope rulings, and hard-wall overruns.
4. Governance defaults baked into every future dispatch: per-run hard
   wall (default 4h), repair-loop budget (2 consecutive failed rounds on
   the same error class => blocker report, not round 3), rescue-commit
   before any forced finalize, WSL never pushes (local pulls + pushes,
   pathspec-only commits).

Reason:
The token cost of the workflow was never Hermes itself; it was the
feedback latency around it — opaque run states caused repeated deep
probes, long catch-up summaries, and late interventions (TASK-023 burned
~20h before closure rules existed). Replacing Hermes would rewrite
working plumbing without fixing latency; adding the sentinel layer fixes
latency without touching what works. Semi-automatic relay (sentinel
prepares, human/local Codex confirms) is chosen over full automation
because 18 days before the deadline, a wrong automatic dispatch chain
costs more than a 5-minute confirmation.
