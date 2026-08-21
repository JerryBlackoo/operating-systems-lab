use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sched::process::{Pid, ProcessControlBlock};

static CLDBG_TIMER_SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn cldbg_enabled() -> bool {
    crate::cmdline::cldbg_enabled()
}

pub(crate) fn cldbg_timer_skip_permitted() -> bool {
    cldbg_enabled() && CLDBG_TIMER_SKIP_COUNT.fetch_add(1, Ordering::Relaxed) < 64
}

fn wtrace_enabled() -> bool {
    crate::cmdline::wtrace_enabled()
}

pub(crate) fn wtrace_save_enabled() -> bool {
    crate::cmdline::wtrace_save_enabled()
}

pub(crate) fn wtrace_trap(label: &str, pid: Pid, cx: &crate::arch::TrapContext) {
    if !wtrace_enabled() {
        return;
    }
    #[cfg(target_arch = "riscv64")]
    {
        crate::println!(
            "[wtrace] {} pid={} sepc={:#x} ra={:#x} sp={:#x} a0={:#x} a1={:#x} a3={:#x} a7={:#x} sstatus={:#x}",
            label, pid, cx.sepc, cx.x[1], cx.x[2], cx.x[10], cx.x[11], cx.x[13], cx.x[17], cx.sstatus,
        );
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::println!(
            "[wtrace] {} pid={} era={:#x} ra={:#x} sp={:#x} a0={:#x} a1={:#x} a3={:#x} a7={:#x} prmd={:#x} ksp={:#x}",
            label, pid, cx.sepc, cx.x[1], cx.x[3], cx.x[4], cx.x[5], cx.x[7], cx.x[11], cx.sstatus, cx.kernel_sp,
        );
    }
}

pub(crate) fn wtrace_process(label: &str, pid: Pid, process: &ProcessControlBlock) {
    if !wtrace_enabled() {
        return;
    }
    crate::println!(
        "[wtrace] {} pid={} state={:?} waiting_for={:?} wait_pgid={:?} pending={:#x} mask={:#x} wait_status_ptr={:#x} wait_ru_ptr={:#x} sigwait_ctx={} waitid={} wchan={:?}",
        label,
        pid,
        process.state,
        process.waiting_for,
        process.wait_pgid,
        process.pending_signals,
        process.signal_mask,
        process.wait_exit_code_ptr,
        process.wait_rusage_ptr,
        process.signal_wait_trap_context.is_some(),
        process.wait_is_waitid,
        process.wait_channel,
    );
}
