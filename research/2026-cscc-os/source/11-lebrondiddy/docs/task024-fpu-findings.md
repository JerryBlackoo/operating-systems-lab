# TASK-024 glibc-RV ROOT CAUSE — hard-float FPU (Claude, 2026-06-15)

## THE ROOT CAUSE (confirmed by instrumented QEMU runs)
The ENTIRE glibc-RV column (basic/busybox/lua/libcbench = 0 on the judge) dies from ONE cause:
**glibc-RV binaries are statically-linked HARD-FLOAT (double-float ABI, rv64gc); musl-RV binaries
are SOFT-FLOAT.** The kernel does not properly support hard-float user programs on RISC-V.

Evidence:
- `file /glibc/busybox` -> "statically linked, double-float ABI"; `/musl/busybox` -> "soft-float".
- Instrumented crash: glibc busybox `ret`s to ra=0xfffffffe7fffffff (a double-NaN bit pattern) ->
  instruction fetch at 0xfffffffe7ffffffe -> SEGV (exit 139), 0 cases. musl runs all 32.
- Original kernel: user `sstatus.FS=Off` -> every glibc FP instruction traps as IllegalInstruction
  -> `task_ilegal()` (kernel/kernel/src/user/mod.rs) SKIPS it (`sepc += 2`) instead of executing it
  -> the FP write never happens -> FP regs hold garbage -> moved into ra -> ret to garbage.
- musl never executes FP -> never traps -> works. This is the whole glibc-RV / hard-float story.

## WIP FIX (branch wip/TASK-024-fpu-claude @ b4b27b3) — gets further but does NOT pass yet
1. Enable the FPU: `riscv::register::sstatus::set_fs(FS::Clean)` in
   crates/polyhal-trap/src/trap/riscv64.rs::init(). CONFIRMED (DBG showed user sstatus.fs=Clean).
2. Full FP context save/restore (f0-f31 + fcsr) at the trap boundary: macros SAVE_FP_REGS /
   LOAD_FP_REGS in crates/polyhal-trap/src/trap/riscv64/macros.rs (wrapped in `.option arch,+d`
   because the integrated assembler needs the D extension enabled explicitly), called from
   uservec / user_restore; TrapFrame.fsx widened [2]->[33] in trapframe/riscv64.rs.

## REMAINING ISSUE (next session)
With FS=Clean + FP saved/restored at the trap boundary, the crash MORPHS: glibc jumps to a KERNEL
address (0xffffffc080270xxx) instead of a NaN. Interpretation: **the kernel is itself built
hard-float (rv64gc lp64d) and uses FP registers in its own Rust code.** FP state must be managed not
only across the user/kernel trap boundary but also across the COOPERATIVE ASYNC EXECUTOR's task
switches (which happen in Rust, not via the trap). The crash is also TIMING-DEPENDENT (a debug
println in the fault path flipped crash<->hang) — consistent with FP racing across async switches.

Likely cleanest fix to try FIRST: **build the kernel SOFT-FLOAT** (RUSTFLAGS target features
`-f,-d` / a soft-float ABI for the kernel crate) so the kernel never touches FP regs; then user FP
only needs the trap-boundary save/restore already written. If that ISA/ABI combo is awkward, the
alternative is full FP save in the executor's context switch (lazy-FP).

## How to iterate (fast, ~10-30s/cycle once cargo is warm)
- Repro images on remote: /tmp/task024-gate/sdcard-rv-basic.img (+ -la).
- Makefile GOTCHA: `kernel-rv` only depends on mount.img, so `make all`/`make kernel-rv` is a NO-OP
  if the artifact exists. ALWAYS `rm -f kernel-rv` first to force a rebuild. (Mutagen also sets
  source mtimes so cargo skips rebuilds — `touch` sources if needed.)
- Build+run rv only:
  `rm -f kernel-rv && docker run --rm --network none -v $PWD:/work -v /tmp/task024-gate:/images -w /work zhouzhouyi/os-contest:20260510 bash -lc 'export RUSTUP_TOOLCHAIN=nightly-2025-01-18 CARGO_NET_OFFLINE=true; make kernel-rv; TEST_IMAGE=/images/sdcard-rv-basic.img TEST_IMAGE_LAYOUT=nested EXPECTED_GROUPS=basic-glibc,basic-musl TIMEOUT_SECONDS=70 STALL_TIMEOUT_SECONDS=60 harness/run-rv.sh'`
- `debugfs` on the repro images needs `-c` (the ext4 bitmap is dirtied by QEMU).
- The wip branch still has DBG-* prints (exec.rs, user/mod.rs) — useful for iterating; strip before shipping.

## VALUE
Cracking this unlocks the WHOLE glibc-RV column (~+200: basic 102 + busybox + lua + libcbench) plus
glibc perf/LTP that the same hard-float crash blocked. High ROI; needs the kernel-FP rework above.
