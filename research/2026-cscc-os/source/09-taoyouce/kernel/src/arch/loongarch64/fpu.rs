//! LoongArch64 user FP/LSX/LASX context management.
//!
//! EUEN enables all three units, so every task context must preserve the full
//! 256-bit LASX register file. The hardware register file and its owner are
//! per hart; saved task state is global so a task can migrate after its old
//! hart has saved it at the return-to-user handoff.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

const NO_OWNER: usize = usize::MAX;

static FPU_OWNERS: [AtomicUsize; crate::sched::smp::MAX_CPUS] =
    [const { AtomicUsize::new(NO_OWNER) }; crate::sched::smp::MAX_CPUS];

/// pid -> latest saved LASX context. A live owner may be newer than this slot.
static FPU_STATES: SpinLock<BTreeMap<usize, Box<FpuState>>> = SpinLock::new(BTreeMap::new());

/// Full LASX view: four u64 lanes for each of the 32 vector registers.
///
/// The explicit 32-byte alignment is required by `xvst`/`xvld`. The UAPI
/// signal payload is serialized field-by-field and does not use this Rust
/// type's tail padding.
#[repr(C, align(32))]
#[derive(Clone)]
pub struct FpuState {
    pub regs: [u64; 4 * 32],
    pub fcc: u64,
    pub fcsr: u32,
    reserved: u32,
}

impl FpuState {
    pub const fn zeroed() -> Self {
        Self {
            regs: [0; 4 * 32],
            fcc: 0,
            fcsr: 0,
            reserved: 0,
        }
    }

    fn boxed_zeroed() -> Box<Self> {
        Box::new(Self::zeroed())
    }
}

static ZERO_FPU_STATE: FpuState = FpuState::zeroed();

#[inline(always)]
fn current_hart_slot() -> usize {
    let hart = crate::arch::asm_helpers::read_cpu_id();
    if hart < crate::sched::smp::MAX_CPUS {
        hart
    } else {
        0
    }
}

#[inline(always)]
fn current_owner() -> &'static AtomicUsize {
    &FPU_OWNERS[current_hart_slot()]
}

/// Save the complete hardware LASX register file into `state`.
unsafe fn save_hw(state: &mut FpuState) {
    let p = state.regs.as_mut_ptr();
    core::arch::asm!(
        "xvst $xr0,  {p}, 0x000",
        "xvst $xr1,  {p}, 0x020",
        "xvst $xr2,  {p}, 0x040",
        "xvst $xr3,  {p}, 0x060",
        "xvst $xr4,  {p}, 0x080",
        "xvst $xr5,  {p}, 0x0a0",
        "xvst $xr6,  {p}, 0x0c0",
        "xvst $xr7,  {p}, 0x0e0",
        "xvst $xr8,  {p}, 0x100",
        "xvst $xr9,  {p}, 0x120",
        "xvst $xr10, {p}, 0x140",
        "xvst $xr11, {p}, 0x160",
        "xvst $xr12, {p}, 0x180",
        "xvst $xr13, {p}, 0x1a0",
        "xvst $xr14, {p}, 0x1c0",
        "xvst $xr15, {p}, 0x1e0",
        "xvst $xr16, {p}, 0x200",
        "xvst $xr17, {p}, 0x220",
        "xvst $xr18, {p}, 0x240",
        "xvst $xr19, {p}, 0x260",
        "xvst $xr20, {p}, 0x280",
        "xvst $xr21, {p}, 0x2a0",
        "xvst $xr22, {p}, 0x2c0",
        "xvst $xr23, {p}, 0x2e0",
        "xvst $xr24, {p}, 0x300",
        "xvst $xr25, {p}, 0x320",
        "xvst $xr26, {p}, 0x340",
        "xvst $xr27, {p}, 0x360",
        "xvst $xr28, {p}, 0x380",
        "xvst $xr29, {p}, 0x3a0",
        "xvst $xr30, {p}, 0x3c0",
        "xvst $xr31, {p}, 0x3e0",
        p = in(reg) p,
        options(nostack)
    );

    let fcsr: u32;
    core::arch::asm!("movfcsr2gr {0}, $fcsr0", out(reg) fcsr, options(nomem, nostack));
    state.fcsr = fcsr;

    let fcc: u64;
    core::arch::asm!(
        "movcf2gr {acc}, $fcc7",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc6",
        "or {acc}, {acc}, {tmp}",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc5",
        "or {acc}, {acc}, {tmp}",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc4",
        "or {acc}, {acc}, {tmp}",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc3",
        "or {acc}, {acc}, {tmp}",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc2",
        "or {acc}, {acc}, {tmp}",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc1",
        "or {acc}, {acc}, {tmp}",
        "slli.d {acc}, {acc}, 8",
        "movcf2gr {tmp}, $fcc0",
        "or {acc}, {acc}, {tmp}",
        acc = out(reg) fcc,
        tmp = out(reg) _,
        options(nomem, nostack)
    );
    state.fcc = fcc;
}

/// Restore the complete hardware LASX register file from `state`.
unsafe fn restore_hw(state: &FpuState) {
    let p = state.regs.as_ptr();
    core::arch::asm!(
        "xvld $xr0,  {p}, 0x000",
        "xvld $xr1,  {p}, 0x020",
        "xvld $xr2,  {p}, 0x040",
        "xvld $xr3,  {p}, 0x060",
        "xvld $xr4,  {p}, 0x080",
        "xvld $xr5,  {p}, 0x0a0",
        "xvld $xr6,  {p}, 0x0c0",
        "xvld $xr7,  {p}, 0x0e0",
        "xvld $xr8,  {p}, 0x100",
        "xvld $xr9,  {p}, 0x120",
        "xvld $xr10, {p}, 0x140",
        "xvld $xr11, {p}, 0x160",
        "xvld $xr12, {p}, 0x180",
        "xvld $xr13, {p}, 0x1a0",
        "xvld $xr14, {p}, 0x1c0",
        "xvld $xr15, {p}, 0x1e0",
        "xvld $xr16, {p}, 0x200",
        "xvld $xr17, {p}, 0x220",
        "xvld $xr18, {p}, 0x240",
        "xvld $xr19, {p}, 0x260",
        "xvld $xr20, {p}, 0x280",
        "xvld $xr21, {p}, 0x2a0",
        "xvld $xr22, {p}, 0x2c0",
        "xvld $xr23, {p}, 0x2e0",
        "xvld $xr24, {p}, 0x300",
        "xvld $xr25, {p}, 0x320",
        "xvld $xr26, {p}, 0x340",
        "xvld $xr27, {p}, 0x360",
        "xvld $xr28, {p}, 0x380",
        "xvld $xr29, {p}, 0x3a0",
        "xvld $xr30, {p}, 0x3c0",
        "xvld $xr31, {p}, 0x3e0",
        p = in(reg) p,
        options(nostack)
    );

    core::arch::asm!(
        "movgr2fcsr $fcsr0, {0}",
        in(reg) state.fcsr as u64,
        options(nomem, nostack)
    );
    core::arch::asm!(
        "movgr2cf $fcc0, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc1, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc2, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc3, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc4, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc5, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc6, {cond}",
        "srli.d {cond}, {cond}, 8",
        "movgr2cf $fcc7, {cond}",
        cond = inout(reg) state.fcc => _,
        options(nomem, nostack)
    );
}

/// Ensure this hart's hardware vector state belongs to `next_pid`.
pub fn switch_to(next_pid: usize) {
    let owner_slot = current_owner();
    if owner_slot.load(Ordering::Relaxed) == next_pid {
        return;
    }

    let mut states = FPU_STATES.lock();
    // Lifecycle operations can invalidate a stale owner while this hart waits.
    let owner = owner_slot.load(Ordering::Acquire);
    if owner == next_pid {
        return;
    }
    if owner != NO_OWNER {
        let state = states.entry(owner).or_insert_with(FpuState::boxed_zeroed);
        unsafe { save_hw(state) };
    }
    let next = states
        .get(&next_pid)
        .map_or(&ZERO_FPU_STATE, |state| state.as_ref());
    unsafe { restore_hw(next) };
    owner_slot.store(next_pid, Ordering::Release);
}

/// Snapshot the interrupted task before a user signal handler can modify LASX.
pub fn snapshot_for_signal(pid: usize) -> Box<FpuState> {
    let owner_slot = current_owner();
    let mut states = FPU_STATES.lock();
    if owner_slot.load(Ordering::Acquire) == pid {
        let state = states.entry(pid).or_insert_with(FpuState::boxed_zeroed);
        unsafe { save_hw(state) };
    }
    states
        .get(&pid)
        .cloned()
        .unwrap_or_else(FpuState::boxed_zeroed)
}

/// Replace the signal handler's hardware state with the state selected by the
/// user signal frame. The handler state is intentionally discarded.
pub fn restore_from_signal(pid: usize, state: Box<FpuState>) {
    let owner_slot = current_owner();
    let mut states = FPU_STATES.lock();
    let owner = owner_slot.load(Ordering::Acquire);
    if owner != NO_OWNER && owner != pid {
        let old = states.entry(owner).or_insert_with(FpuState::boxed_zeroed);
        unsafe { save_hw(old) };
    }
    states.insert(pid, state);
    unsafe { restore_hw(states.get(&pid).expect("signal FPU state disappeared")) };
    owner_slot.store(pid, Ordering::Release);
}

/// fork/clone inherits the parent's latest vector context.
pub fn fork_inherit(parent_pid: usize, child_pid: usize) {
    let owner_slot = current_owner();
    let mut states = FPU_STATES.lock();
    if owner_slot.load(Ordering::Acquire) == parent_pid {
        let parent = states
            .entry(parent_pid)
            .or_insert_with(FpuState::boxed_zeroed);
        unsafe { save_hw(parent) };
    }
    if let Some(parent) = states.get(&parent_pid).cloned() {
        states.insert(child_pid, parent);
    }
}

/// execve clears every stale owner copy and gives the current hart a zero state.
pub fn exec_reset(pid: usize) {
    let current_hart = current_hart_slot();
    let mut states = FPU_STATES.lock();
    states.remove(&pid);
    for (hart, owner) in FPU_OWNERS.iter().enumerate() {
        if owner.load(Ordering::Acquire) == pid {
            if hart == current_hart {
                unsafe { restore_hw(&ZERO_FPU_STATE) };
                owner.store(pid, Ordering::Release);
            } else {
                owner.store(NO_OWNER, Ordering::Release);
            }
        }
    }
}

/// Drop saved state and invalidate stale hardware ownership on every hart.
pub fn on_exit(pid: usize) {
    let mut states = FPU_STATES.lock();
    states.remove(&pid);
    for owner in &FPU_OWNERS {
        if owner.load(Ordering::Acquire) == pid {
            owner.store(NO_OWNER, Ordering::Release);
        }
    }
}
