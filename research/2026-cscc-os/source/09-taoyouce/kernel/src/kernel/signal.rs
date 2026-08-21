use alloc::vec::Vec;

use crate::mm::{PTEFlags, VirtAddr};

use crate::syscall::remove_pid_from_futex_waiters;
use crate::syscall::user::{read_user_buffer, write_user_buffer};

pub(super) const SIGINFO_SIZE: usize = 128;
pub(super) const SIGINFO_BUF_LEN: usize = 128;

#[cfg(target_arch = "riscv64")]
pub(super) const UCONTEXT_SIZE: usize = 960;

#[cfg(target_arch = "loongarch64")]
pub(super) const UCONTEXT_SIZE: usize = 448;

#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_SC_FLAGS_OFFSET: usize = 0x1b8;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_EXTCONTEXT_OFFSET: usize = 0x1c0;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_SC_USED_FP: u32 = 1;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_FPU_CTX_MAGIC: u32 = 0x4650_5501;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_LSX_CTX_MAGIC: u32 = 0x5358_0001;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_LASX_CTX_MAGIC: u32 = 0x4153_5801;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_FPU_RECORD_SIZE: usize = 16 + 272;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_LSX_RECORD_SIZE: usize = 16 + 528;
#[cfg(target_arch = "loongarch64")]
pub(super) const LOONGARCH_LASX_RECORD_SIZE: usize = 16 + 1040;

#[cfg(target_arch = "loongarch64")]
const SIGNAL_EXTCONTEXT_SIZE: usize = LOONGARCH_LASX_RECORD_SIZE + 16;
#[cfg(not(target_arch = "loongarch64"))]
const SIGNAL_EXTCONTEXT_SIZE: usize = 0;

#[inline]
fn signal_frame_sp(base_sp: usize, frame_size: usize) -> Option<usize> {
    let candidate = base_sp.checked_sub(frame_size)?;
    #[cfg(target_arch = "loongarch64")]
    {
        // sc_extcontext starts 576 bytes after the frame base. Making the
        // frame base 16 mod 32 aligns the LASX payload (header + 16) to 32.
        candidate.checked_sub(16).map(|sp| (sp & !31) + 16)
    }
    #[cfg(not(target_arch = "loongarch64"))]
    {
        Some(candidate & !15)
    }
}

#[cfg(target_arch = "loongarch64")]
fn write_loongarch_fpu_signal_frame(
    addr_space: &mut crate::mm::AddressSpace,
    ucontext_ptr: usize,
    state: &crate::arch::loongarch64::fpu::FpuState,
) {
    write_u64_to_user_as(
        addr_space,
        ucontext_ptr + LOONGARCH_SC_FLAGS_OFFSET,
        LOONGARCH_SC_USED_FP as u64,
    );

    let info = ucontext_ptr + LOONGARCH_EXTCONTEXT_OFFSET;
    let header = [
        LOONGARCH_LASX_CTX_MAGIC as u64
            | ((LOONGARCH_LASX_RECORD_SIZE as u64) << 32),
        0,
    ];
    write_u64_batch_to_user(addr_space, info, &header);
    write_u64_batch_to_user(addr_space, info + 16, &state.regs);
    write_u64_batch_to_user(
        addr_space,
        info + 16 + state.regs.len() * 8,
        &[state.fcc, state.fcsr as u64],
    );
    write_u64_batch_to_user(
        addr_space,
        info + LOONGARCH_LASX_RECORD_SIZE,
        &[0, 0],
    );
}

#[cfg(target_arch = "riscv64")]
pub(super) const RT_SIGRETURN_TRAMPOLINE: u64 = 0x0000_0073_08b0_0893;

#[cfg(target_arch = "loongarch64")]
pub(super) const RT_SIGRETURN_TRAMPOLINE: u64 = 0x002b_0000_0282_2c0b;

pub(super) const SA_RESTORER: usize = 0x0400_0000;

/// 共享 rt_sigreturn trampoline 页的固定用户虚地址（对标 Linux 的 vdso
/// `__vdso_rt_sigreturn`）。全局唯一物理帧按需以 R+X 只读映射到各地址空间；
/// libc 未提供 SA_RESTORER 时优先指向此页，代替在用户栈页写指令 + 加 X 映射。
/// 可执行页在 QEMU TCG 下受自修改代码保护：页内任何写（信号帧写入、handler
/// 自身的栈操作）都会走慢路径并使翻译块失效——栈页恰是最高频写页，代价极大；
/// 真机上同样省去每次投递的指令写入与 icache 同步。地址选在 mmap 自动分配
/// 窗口与用户栈之外；若被显式 MAP_FIXED 占用则回退栈 trampoline 路径。
#[cfg(target_arch = "riscv64")]
pub(super) const SIGRETURN_TRAMPOLINE_VA: usize = 0x3F_FFFF_F000;

#[cfg(target_arch = "loongarch64")]
pub(super) const SIGRETURN_TRAMPOLINE_VA: usize = 0x7F_FFFF_0000;

static SIGRETURN_FRAME: crate::sync::SpinLock<Option<crate::mm::FrameTracker>> =
    crate::sync::SpinLock::new(None);

/// 惰性初始化全局 trampoline 帧（内容 = rt_sigreturn ecall 序列，写入一次后
/// 永不再写，帧由全局 static 持有故永不回收）。
fn sigreturn_trampoline_ppn() -> Option<crate::mm::PhysPageNum> {
    let mut g = SIGRETURN_FRAME.lock();
    if g.is_none() {
        let frame = crate::mm::frame_alloc()?;
        frame.ppn.as_bytes_mut()[..8].copy_from_slice(&RT_SIGRETURN_TRAMPOLINE.to_le_bytes());
        crate::arch::fence_i();
        *g = Some(frame);
    }
    g.as_ref().map(|f| f.ppn)
}

/// sigreturn trampoline 固定虚地址（LA：栈顶之上 64KB 的保留区；RV：低区保留）。
/// 该页不走 VMA（对标 Linux sigpage/__kernel_sigreturn 的固定内核映射），
/// 由 `ensure_sigreturn_page` 惰性装入各地址空间、`release_user_frames` 退出时清除。
pub fn sigreturn_trampoline_va() -> usize {
    SIGRETURN_TRAMPOLINE_VA
}

/// 把 trampoline 页映射进给定地址空间（幂等）。返回可执行的用户虚地址；
/// 固定虚地址已被其他映射占用时返回 None，调用方回退栈 trampoline。
pub(super) fn ensure_sigreturn_page(addr_space: &mut crate::mm::AddressSpace) -> Option<usize> {
    let ppn = sigreturn_trampoline_ppn()?;
    let vpn = VirtAddr(SIGRETURN_TRAMPOLINE_VA).floor();
    match addr_space.page_table().translate(vpn) {
        Some(mapped) if mapped == ppn => Some(SIGRETURN_TRAMPOLINE_VA),
        Some(_) => None,
        None => {
            addr_space
                .page_table_mut()
                .map(vpn, ppn, PTEFlags::R | PTEFlags::X | PTEFlags::U);
            Some(SIGRETURN_TRAMPOLINE_VA)
        }
    }
}

#[inline]
pub(super) fn select_signal_restorer(flags: usize, restorer: usize, trampoline: usize) -> usize {
    if flags & SA_RESTORER != 0 && restorer != 0 {
        restorer
    } else {
        trampoline
    }
}

pub(super) fn default_ignored_signal(signum: usize) -> bool {
    matches!(signum, 17 | 23 | 28)
}

pub(super) fn build_si_user_siginfo(signum: i32, sender_pid: usize, sender_uid: u32) -> [u8; SIGINFO_BUF_LEN] {
    // si_code = SI_USER(0)
    build_si_siginfo(signum, sender_pid, sender_uid, 0)
}

/// 构造 SI_USER / SI_TKILL 等"由进程发送"的 siginfo_t：
/// si_signo@0, si_code@8, si_pid@16, si_uid@20（LP64 generic 布局）。
pub(super) fn build_si_siginfo(signum: i32, sender_pid: usize, sender_uid: u32, si_code: i32) -> [u8; SIGINFO_BUF_LEN] {
    let mut buf = [0u8; SIGINFO_BUF_LEN];
    buf[0..4].copy_from_slice(&signum.to_ne_bytes());
    buf[8..12].copy_from_slice(&si_code.to_ne_bytes());
    buf[16..20].copy_from_slice(&(sender_pid as i32).to_ne_bytes());
    buf[20..24].copy_from_slice(&sender_uid.to_ne_bytes());
    buf
}

pub(super) fn pop_pending_siginfo_for(
    process: &mut crate::sched::process::ProcessControlBlock,
    signum: i32,
) -> Option<[u8; SIGINFO_BUF_LEN]> {
    let pos = process
        .pending_siginfo
        .iter()
        .position(|(sig, _)| *sig == signum);
    pos.and_then(|idx| process.pending_siginfo.remove(idx).map(|(_, b)| b))
}

pub(super) fn enqueue_pending_siginfo_process(
    process: &mut crate::sched::process::ProcessControlBlock,
    sig: i32,
    info: [u8; SIGINFO_BUF_LEN],
) {
    const PENDING_SIGINFO_MAX: usize = 32;
    if process.pending_siginfo.len() >= PENDING_SIGINFO_MAX {
        process.pending_siginfo.pop_front();
    }
    process.pending_siginfo.push_back((sig, info));
}

pub(super) fn clear_pending_signal_if_no_payload(
    process: &mut crate::sched::process::ProcessControlBlock,
    signum: usize,
    sig_bit: u64,
) {
    let signo = signum as i32;
    if !process.pending_signal_fds.iter().any(|(sig, _)| *sig == signo)
        && !process.pending_siginfo.iter().any(|(sig, _)| *sig == signo)
    {
        process.pending_signals &= !sig_bit;
    }
}

pub(super) fn current_user_write_range_ready(user_va: usize, len: usize) -> bool {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(t) = table.as_mut() else { return false };
    let Some(pid) = t.current else { return false };
    let as_pid = match t.get(pid) {
        Some(p) if p.mm.is_some() => pid,
        Some(p) => p.tgid,
        None => return false,
    };
    let Some(process) = t.get(as_pid) else { return false };
    let Some(ref mm) = process.mm else { return false };
    let mut addr_space = mm.lock();
    fault_in_user_write_range(&mut *addr_space, user_va, len)
}

pub(crate) fn pending_interrupting_signal(pid: usize) -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        let (mut deliverable, leader_pid) = match t.get(pid) {
            Some(process) => (
                process.pending_signals & !process.signal_mask,
                if process.is_thread { process.tgid } else { pid },
            ),
            None => return false,
        };
        while deliverable != 0 {
            let sig_bit = deliverable & deliverable.wrapping_neg();
            let signum = sig_bit.trailing_zeros() as usize + 1;
            let action = t
                .get(leader_pid)
                .and_then(|leader| leader.signal_handlers.get(signum - 1).copied())
                .unwrap_or(crate::sched::process::SignalAction {
                    handler: 0,
                    flags: 0,
                    restorer: 0,
                    mask: 0,
                });
            if action.handler != 1 && !(action.handler == 0 && default_ignored_signal(signum)) {
                return true;
            }
            deliverable &= !sig_bit;
        }
    }
    false
}

/// signalfd 语义（Linux）：signalfd 读的是**当前进程的 pending 信号集**里
/// 命中 fd mask 的信号（通常调用方已用 sigprocmask 阻塞它们）。
/// 只判可读性，不消费。
pub(crate) fn signalfd_pending(mask: u64) -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get(pid) {
                return process.pending_signals & mask != 0;
            }
        }
    }
    false
}

/// 从当前进程 pending 集里取出一个命中 mask 的信号并消费之。
/// 返回 (signo, si_code, si_pid, si_uid, si_status)。
pub(crate) fn signalfd_dequeue(mask: u64) -> Option<(u32, i32, u32, u32, i32)> {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_mut()?;
    let pid = t.current?;
    let process = t.get_mut(pid)?;
    let hits = process.pending_signals & mask;
    if hits == 0 {
        return None;
    }
    let sig_bit = hits & hits.wrapping_neg();
    let signum = sig_bit.trailing_zeros() as usize + 1;
    let info = pop_pending_siginfo_for(process, signum as i32);
    clear_pending_signal_if_no_payload(process, signum, sig_bit);
    if let Some(buf) = info {
        let code = i32::from_ne_bytes(buf[8..12].try_into().unwrap());
        let spid = i32::from_ne_bytes(buf[16..20].try_into().unwrap()) as u32;
        let suid = u32::from_ne_bytes(buf[20..24].try_into().unwrap());
        let status = i32::from_ne_bytes(buf[24..28].try_into().unwrap());
        Some((signum as u32, code, spid, suid, status))
    } else {
        process.pending_signals &= !sig_bit;
        Some((signum as u32, 0, 0, 0, 0))
    }
}

pub(crate) fn pending_interrupting_signal_for_process(
    table: &crate::sched::process::ProcessTable,
    pid: usize,
    wait_mask: u64,
) -> bool {
    next_deliverable_signal_action(table, pid, wait_mask).is_some()
}

/// 返回第一个可投递（未屏蔽且 disposition 非 ignore）的信号及其 action。
/// handler 查找遵循 CLONE_SIGHAND 共享语义：线程取 tgid leader 的 handler 表。
pub(crate) fn next_deliverable_signal_action(
    table: &crate::sched::process::ProcessTable,
    pid: usize,
    wait_mask: u64,
) -> Option<(usize, crate::sched::process::SignalAction)> {
    let (mut deliverable, leader_pid) = match table.get(pid) {
        Some(process) => (
            process.pending_signals & !wait_mask & !process.signal_mask,
            if process.is_thread { process.tgid } else { pid },
        ),
        None => return None,
    };
    while deliverable != 0 {
        let sig_bit = deliverable & deliverable.wrapping_neg();
        let signum = sig_bit.trailing_zeros() as usize + 1;
        let action = table
            .get(leader_pid)
            .and_then(|leader| leader.signal_handlers.get(signum - 1).copied())
            .unwrap_or(crate::sched::process::SignalAction {
                handler: 0,
                flags: 0,
                restorer: 0,
                mask: 0,
            });
        if action.handler != 1 && !(action.handler == 0 && default_ignored_signal(signum)) {
            return Some((signum, action));
        }
        deliverable &= !sig_bit;
    }
    None
}

pub(super) fn fault_in_user_write_range(
    addr_space: &mut crate::mm::AddressSpace,
    start: usize,
    len: usize,
) -> bool {
    use crate::mm::{PAGE_SIZE, VirtAddr};
    if len == 0 {
        return true;
    }
    let end = match start.checked_add(len - 1) {
        Some(end) => end,
        None => return false,
    };
    let mut page = start & !(PAGE_SIZE - 1);
    let last = end & !(PAGE_SIZE - 1);
    loop {
        let va = VirtAddr(page);
        if addr_space.page_table().translate(va.floor()).is_none()
            && !addr_space.handle_page_fault(va, true)
        {
            return false;
        }
        if page == last {
            break;
        }
        page += PAGE_SIZE;
    }
    true
}

/// 同步信号调试打印预算：同步致命信号若原地重抛会形成高频循环，无预算打印会
/// 淹没串口并改变时序。EXECDBG 关闭时零成本，开启时只打前 N 条。
static SIGDBG_BUDGET: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(240);

pub(crate) fn sigdbg_budget_ok() -> bool {
    SIGDBG_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |n| if n == 0 { None } else { Some(n - 1) },
        )
        .is_ok()
}

/// A synchronous fault (page fault → SIGSEGV/SIGBUS, illegal instruction →
/// SIGILL, etc.) wants to be delivered to the faulting task. If the task (its
/// `CLONE_SIGHAND` leader) has a real user handler installed for `sig` and the
/// signal is not currently blocked, mark it pending and return `true`; the
/// trap-return path's `check_and_deliver_signals` then builds the handler frame
/// so userspace can recover (e.g. mprotect04 catches SIGSEGV and siglongjmps).
///
/// Returns `false` when no catchable handler applies (SIG_DFL/SIG_IGN, blocked,
/// or no current task). Linux forces the default action for a synchronous
/// signal that is ignored or blocked, so the caller should terminate the task
/// by signal in that case.
pub fn try_queue_fault_signal(sig: i32, fault_addr: usize, si_code: i32) -> bool {
    if sig < 1 || sig > 64 {
        return false;
    }
    let bit = 1u64 << (sig as usize - 1);
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() {
        Some(t) => t,
        None => return false,
    };
    let pid = match t.current {
        Some(p) => p,
        None => return false,
    };
    let leader = t
        .get(pid)
        .map(|p| if p.is_thread { p.tgid } else { pid })
        .unwrap_or(pid);
    let handler = t
        .get(leader)
        .map(|p| p.signal_handlers[sig as usize - 1].handler)
        .unwrap_or(0);
    // SIG_DFL(0) / SIG_IGN(1) are not catchable: force default action.
    if handler == 0 || handler == 1 {
        if crate::cmdline::execdbg_enabled() && sigdbg_budget_ok() {
            crate::println!(
                "[EXECDBG] faultsig pid={} leader={} sig={} handler={:#x} -> force default (terminate)",
                pid, leader, sig, handler
            );
        }
        return false;
    }
    let blocked = t
        .get(pid)
        .map(|p| p.signal_mask & bit != 0)
        .unwrap_or(false);
    if blocked {
        if crate::cmdline::execdbg_enabled() {
            crate::println!(
                "[EXECDBG] faultsig pid={} sig={} blocked -> force default (terminate)",
                pid, sig
            );
        }
        return false;
    }
    if let Some(p) = t.get_mut(pid) {
        p.pending_signals |= bit; crate::sched::process::note_signal_hint();
        // Carry the faulting address and fault cause so the SA_SIGINFO handler
        // sees a correct `siginfo_t` (the generic LP64 _sigfault layout places
        // si_code@8 and si_addr@16). The caller passes the architecture-derived
        // si_code (SEGV_MAPERR vs SEGV_ACCERR / BUS_ADRERR). Without this the
        // handler reads si_addr==0 (hugemmap23).
        let info = build_fault_siginfo(sig, si_code, fault_addr);
        enqueue_pending_siginfo_process(p, sig, info);
        true
    } else {
        false
    }
}

/// Build a `siginfo_t` for a synchronous memory fault: si_signo@0, si_code@8,
/// and the _sigfault union's `si_addr@16` (generic LP64 layout shared by
/// riscv64 and loongarch64).
pub(super) fn build_fault_siginfo(signum: i32, si_code: i32, fault_addr: usize) -> [u8; SIGINFO_BUF_LEN] {
    let mut buf = [0u8; SIGINFO_BUF_LEN];
    buf[0..4].copy_from_slice(&signum.to_ne_bytes());
    buf[8..12].copy_from_slice(&si_code.to_ne_bytes());
    buf[16..24].copy_from_slice(&(fault_addr as u64).to_ne_bytes());
    buf
}

/// 信号投递 — 在 trap 返回用户态前调用
/// 检查当前进程是否有未屏蔽的待处理信号，若有则修改 TrapContext 跳转到用户态 handler
///
/// CLONE_SIGHAND 语义：线程的 signal_handlers 取自 tgid leader。
pub fn check_and_deliver_signals(cx: &mut crate::arch::TrapContext) {
    let (_, _, handoff_pending) = crate::sched::process_sched::user_return_target();
    if handoff_pending {
        // next 尚未成为 committed current，不能在 previous 栈上执行可能 stop/exit/
        // reschedule 的信号动作；架构 stack-first finalizer 会在提交后重新调用。
        return;
    }
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            #[cfg(target_arch = "riscv64")]
            const CUR_SP_REG: usize = 2;
            #[cfg(target_arch = "loongarch64")]
            const CUR_SP_REG: usize = 3;
            // 先只读地收集决策所需信息，避免多次可变借用冲突
            let (deliverable, tgid, is_thread, stale_handler) = {
                let p = match t.get(pid) { Some(p) => p, None => return };
                let d = p.pending_signals & !p.signal_mask;
                if d == 0 {
                    // 同步致命信号被 mask 挡住而未投递 = 会原地重抛同一异常。
                    // Linux `force_sig_info` 对同步信号强制解除屏蔽并走默认动作，
                    // 绝不允许「不投递也不终止」。
                    if crate::cmdline::execdbg_enabled() && p.pending_signals != 0 {
                        crate::println!(
                            "[EXECDBG] sigdeliver pid={} early=masked pending={:#x} mask={:#x}",
                            pid, p.pending_signals, p.signal_mask
                        );
                    }
                    return;
                }
                if crate::cmdline::execdbg_enabled() {
                    const SYNC_MASK: u64 = (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 10);
                    if d & SYNC_MASK != 0 && sigdbg_budget_ok() {
                        crate::println!(
                            "[EXECDBG] sigdeliver pid={} tgid={} d={:#x} mask={:#x} saved_ctx={} hsp={:#x}",
                            pid, p.tgid, d, p.signal_mask,
                            p.saved_trap_context.is_some(), p.signal_handler_sp
                        );
                    }
                }
                // 同步异常信号：SIGILL(4)/SIGTRAP(5)/SIGBUS(7)/SIGFPE(8)/SIGSEGV(11)。
                // 这些信号源自“用户 PC 当前正在重复触发取指/访存/非法指令异常”，
                // 必须立即投递新帧以推进——若不投递，trap 返回后会原地重抛同一异常，
                // 永久空转死锁。
                const SYNC_FAULTS: u64 = (1 << (4 - 1)) | (1 << (5 - 1))
                    | (1 << (7 - 1)) | (1 << (8 - 1)) | (1 << (11 - 1));
                // 单槽 saved_trap_context 既充当“正在 handler 中”的门（避免覆盖单槽
                // 上下文），但 sigreturn 实际从用户栈上的 ucontext 恢复、不依赖单槽，
                // 故覆盖单槽不破坏 sigreturn 正确性。handler 可能经 longjmp 逃逸而不
                // 调用 sigreturn，单槽永久残留。原 sp 栈序启发式（cur_sp>hsp ⟹ 陈旧）
                // 在 crashme 这类“执行随机代码把 sp 改成任意低值再触发异常”的场景下
                // 失效（cur_sp<=hsp 被误判为仍在 handler），导致同步异常被永久吞掉、
                // 整核死锁。修正：只要有待投递的同步异常信号，一律视为可投递（无论
                // handler 是 longjmp 逃逸还是自身再次故障，都应投递新帧）；仅异步信号
                // 在 handler 运行期间沿用栈序门、等待 sigreturn。
                let stale = if p.saved_trap_context.is_some() {
                    if d & SYNC_FAULTS != 0 {
                        true // 同步异常：强制投递推进，绕过栈序门
                    } else {
                        let cur_sp = cx.x[CUR_SP_REG];
                        if p.signal_handler_sp != 0 && cur_sp > p.signal_handler_sp {
                            true // handler 经 longjmp 放弃，旧上下文陈旧
                        } else {
                            if crate::cmdline::scdbg_sig_enabled() && (d & (1 << 9)) != 0 {
                                crate::println!(
                                    "[sigdbg] swallow pid={} d={:#x} cur_sp={:#x} hsp={:#x}",
                                    pid, d, cx.x[CUR_SP_REG], p.signal_handler_sp
                                );
                            }
                            return; // 仍在 handler 内，等待 sigreturn
                        }
                    }
                } else {
                    false
                };
                (d, p.tgid, p.is_thread, stale)
            };
            // 清除被 siglongjmp 放弃的陈旧 handler 上下文。signal_mask 已由
            // siglongjmp 经 rt_sigprocmask 恢复，这里只丢弃单槽保存值。
            if stale_handler {
                if let Some(p) = t.get_mut(pid) {
                    p.saved_trap_context = None;
                    p.saved_signal_mask = None;
                    p.signal_handler_sp = 0;
                }
            }

            let sig_bit = deliverable & deliverable.wrapping_neg();
            let signum = sig_bit.trailing_zeros() as usize + 1;
            // 从 tgid leader 取 signal handler（CLONE_SIGHAND 共享语义）
            let action = {
                let leader_pid = if is_thread { tgid } else { pid };
                match t.get(leader_pid) {
                    Some(leader) => leader.signal_handlers[signum - 1],
                    None => crate::sched::process::SignalAction { handler: 0, flags: 0, restorer: 0, mask: 0 },
                }
            };
            // ptrace signal-delivery-stop (Linux get_signal → ptrace_signal):
            // a traced task, upon dequeuing any signal other than SIGKILL,
            // enters a stop and notifies its tracer instead of acting on the
            // signal's disposition — even SIG_IGN'd or handler-bearing signals
            // stop the tracee. The tracer then resumes it via PTRACE_CONT
            // (optionally re-injecting a signal) or terminates it via
            // PTRACE_KILL. SIGKILL is never interceptible; SIGSTOP drives the
            // separate job-control group-stop path.
            let traced = t.get(pid).map_or(false, |p| p.ptrace_tracer != 0);
            if traced && signum != 9 && signum != 19 {
                if let Some(process) = t.get_mut(pid) {
                    process.pending_signals &= !sig_bit;
                    let signo = signum as i32;
                    process.pending_signal_fds.retain(|(sig, _)| *sig != signo);
                    process.pending_siginfo.retain(|(sig, _)| *sig != signo);
                    process.ptrace_stopped = true;
                    process.ptrace_stop_reason = 1; // PTRACE_STOP_SIGNAL
                    process.ptrace_pending_sig = 0;
                }
                drop(table);
                crate::sched::process::stop_current_for_signal(signum as i32);
                unreachable!();
            }
            let siginfo_fd = if let Some(process) = t.get_mut(pid) {
                let pos = process.pending_signal_fds
                    .iter()
                    .position(|(sig, _)| *sig == signum as i32);
                let fd = pos.map(|idx| process.pending_signal_fds.remove(idx).1);
                if !process.pending_signal_fds.iter().any(|(sig, _)| *sig == signum as i32) {
                    process.pending_signals &= !sig_bit;
                }
                fd
            } else {
                None
            };

            // Slice 2.0：若该 sig 上挂着 rt_sigqueueinfo 入队的 siginfo_t
            // payload（FIFO），弹出最早一条，后面写 user-frame 时整 128B 覆盖
            // 默认 4×u64 填充，把真实 si_value / si_pid / si_uid / si_code
            // 还原给 SA_SIGINFO handler。
            let queued_siginfo: Option<[u8; SIGINFO_BUF_LEN]> =
                if let Some(process) = t.get_mut(pid) {
                    let pos = process
                        .pending_siginfo
                        .iter()
                        .position(|(s, _)| *s == signum as i32);
                    pos.and_then(|idx| process.pending_siginfo.remove(idx).map(|(_, b)| b))
                } else {
                    None
                };

            if crate::cmdline::execdbg_enabled() {
                const SYNC_MASK: u64 = (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 10);
                if sig_bit & SYNC_MASK != 0 && sigdbg_budget_ok() {
                    crate::println!(
                        "[EXECDBG] sigdispatch pid={} signum={} handler={:#x} flags={:#x} is_thread={} tgid={}",
                        pid, signum, action.handler, action.flags, is_thread, tgid
                    );
                }
            }
            match action.handler {
                0 => {
                    // SIG_DFL
                    match signum {
                        18 => {
                            if let Some(process) = t.get_mut(pid) {
                                process.pending_signals &= !sig_bit;
                            }
                        }
                        19 => {
                            if let Some(process) = t.get_mut(pid) {
                                process.pending_signals &= !sig_bit;
                            }
                            drop(table);
                            crate::sched::process::stop_current_for_signal(19);
                        }
                        1 | 2 | 3 | 6 | 9 | 11 | 13 | 14 | 15 => {
                            drop(table);
                            let sig = signum as i32;
                            let reason = if crate::sched::process::signal_is_core_dumping(sig) {
                                crate::kernel::coredump::do_coredump(sig);
                                crate::sched::process::ExitReason::signaled_with_dump(sig)
                            } else {
                                crate::sched::process::ExitReason::signaled(sig)
                            };
                            crate::sched::process::exit_group_with_reason(reason);
                        }
                        17 | 23 | 28 => {
                            if let Some(process) = t.get_mut(pid) {
                                process.pending_signals &= !sig_bit;
                                let signo = signum as i32;
                                process.pending_signal_fds.retain(|(sig, _)| *sig != signo);
                                process.pending_siginfo.retain(|(sig, _)| *sig != signo);
                            }
                        }
                        _ => {
                            drop(table);
                            let sig = signum as i32;
                            let reason = if crate::sched::process::signal_is_core_dumping(sig) {
                                crate::kernel::coredump::do_coredump(sig);
                                crate::sched::process::ExitReason::signaled_with_dump(sig)
                            } else {
                                crate::sched::process::ExitReason::signaled(sig)
                            };
                            crate::sched::process::exit_group_with_reason(reason);
                        }
                    }
                }
                1 => {
                    if let Some(process) = t.get_mut(pid) {
                        process.pending_signals &= !sig_bit;
                        let signo = signum as i32;
                        process.pending_signal_fds.retain(|(sig, _)| *sig != signo);
                        process.pending_siginfo.retain(|(sig, _)| *sig != signo);
                    }
                }
                handler_addr => {
                    // 用户态信号处理函数
                    if let Some(process) = t.get_mut(pid) {
                        let interrupted_sigsuspend = process.sigsuspend_restore_mask.is_some();
                        if interrupted_sigsuspend {
                            process.signal_wait_trap_context.take();
                        }
                        let mut saved = [0usize; 32];
                        saved.copy_from_slice(&cx.x);
                        saved[0] = cx.sepc;

                        // Linux `rt_sigsuspend()` semantics are subtle: when a
                        // caught signal interrupts the sleep, the handler's
                        // sigreturn must resume user code *after* the
                        // rt_sigsuspend ecall with `-EINTR`, not re-execute the
                        // ecall with its original `a0=mask_ptr`.  Our sleep
                        // path stores a pre-ecall TrapContext so blocked
                        // syscalls can generally restart; for sigsuspend that
                        // would make callers such as LTP fcntl14 go back to
                        // sleep and never run their post-signal unlock path.
                        //
                        // `prepare_trap_context_for_user_return()` may already
                        // have converted the context to `pc_after_ecall/-EINTR`.
                        // If not, recognize the still-original a0 and advance
                        // once here before writing the ucontext frame consumed
                        // by sys_sigreturn().
                        if interrupted_sigsuspend {
                            #[cfg(target_arch = "riscv64")]
                            const SIGSUSPEND_REG_A0: usize = 10;
                            #[cfg(target_arch = "loongarch64")]
                            const SIGSUSPEND_REG_A0: usize = 4;
                            let eintr = (-4isize) as usize;
                            if saved[SIGSUSPEND_REG_A0] != eintr {
                                saved[0] = saved[0].saturating_add(4);
                            }
                            saved[SIGSUSPEND_REG_A0] = eintr;
                        }
                        process.saved_trap_context = Some(saved);
                        // 若处于 sigsuspend 期间，handler 返回后必须恢复 sigsuspend
                        // 之前的真实 mask（Linux 语义）；否则恢复 handler 调用前的当前 mask。
                        let baseline_mask = process
                            .sigsuspend_restore_mask
                            .take()
                            .unwrap_or(process.signal_mask);
                        process.saved_signal_mask = Some(baseline_mask);

                        // Linux `get_signal`/`signal_setup_done`: blocked |=
                        // sa_mask, 且除非 SA_NODEFER 否则再 |= 当前信号自身。
                        // 缺了 SA_NODEFER 处理会把以 SA_NODEFER 装载的 handler
                        // （如 crashme 的 SIGALRM）在经 longjmp 退出（不走
                        // sigreturn 恢复 mask）后永久屏蔽该信号 → 后续永不再投递。
                        const SA_NODEFER: usize = 0x4000_0000;
                        let self_bit = if action.flags & SA_NODEFER != 0 { 0 } else { sig_bit };
                        let unmaskable = (1u64 << 8) | (1u64 << 18);
                        process.signal_mask = (baseline_mask | self_bit | action.mask) & !unmaskable;
                    }

                    #[cfg(target_arch = "riscv64")]
                    const REG_SP: usize = 2;
                    #[cfg(target_arch = "riscv64")]
                    const REG_A0: usize = 10;
                    #[cfg(target_arch = "riscv64")]
                    const REG_A1: usize = 11;
                    #[cfg(target_arch = "riscv64")]
                    const REG_A2: usize = 12;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_SP: usize = 3;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_A0: usize = 4;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_A1: usize = 5;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_A2: usize = 6;
                    let sp = cx.x[REG_SP];
                    let frame_size =
                        (SIGINFO_SIZE + UCONTEXT_SIZE + SIGNAL_EXTCONTEXT_SIZE + 8 + 15) & !15;
                    // sigaltstack(2) Linux 对标：SA_ONSTACK 且备用栈启用时 handler 帧
                    // 建在备用栈上；若已在备用栈上（SS_ONSTACK）再要求建帧 →
                    // force_sigsegv（Linux get_sigframe），否则 SA_NODEFER 的 handler
                    // 自身再故障会无限重入同一 handler（实测 BuildStorm rustc
                    // SIGSEGV handler 打印死循环）。SS_AUTODISARM 时进入 handler 即
                    // 置 SS_DISABLE（后续信号回普通栈，与 Linux 一致）。
                    const SA_ONSTACK: usize = 0x0800_0000;
                    const SS_DISABLE: u32 = 2;
                    const SS_ONSTACK: u32 = 1;
                    const SS_AUTODISARM: u32 = 0x8000_0000;
                    let use_altstack = action.flags & SA_ONSTACK != 0;
                    let base_sp = if use_altstack {
                        let (disabled, on, size, alt_sp, autodisarm) = t
                            .get(pid)
                            .map(|p| {
                                (
                                    p.sigaltstack_flags & SS_DISABLE != 0,
                                    p.sigaltstack_flags & SS_ONSTACK != 0,
                                    p.sigaltstack_size,
                                    p.sigaltstack_sp,
                                    p.sigaltstack_flags & SS_AUTODISARM != 0,
                                )
                            })
                            .unwrap_or((true, false, 0, 0, false));
                        if !disabled && size != 0 {
                            if on {
                                drop(table);
                                crate::sched::process::exit_group_with_reason(
                                    crate::sched::process::ExitReason::signaled_with_dump(11),
                                );
                            }
                            if let Some(process) = t.get_mut(pid) {
                                process.sigaltstack_flags |= SS_ONSTACK;
                                if autodisarm {
                                    process.sigaltstack_flags |= SS_DISABLE;
                                }
                            }
                            alt_sp.saturating_add(size)
                        } else {
                            sp
                        }
                    } else {
                        sp
                    };
                    let new_sp = match signal_frame_sp(base_sp, frame_size) {
                        Some(new_sp) => new_sp,
                        None => {
                            drop(table);
                            // SIGSEGV(11) 是 POSIX core-dumping 信号，显式标 dump。
                            crate::sched::process::exit_group_with_reason(
                                crate::sched::process::ExitReason::signaled_with_dump(11),
                            );
                        }
                    };
                    let siginfo_ptr = new_sp;
                    let ucontext_ptr = new_sp + SIGINFO_SIZE;
                    let trampoline_ptr = new_sp + frame_size - 8;
                    let signal_restorer = select_signal_restorer(action.flags, action.restorer, trampoline_ptr);

                    cx.sepc = handler_addr;
                    cx.x[REG_A0] = signum;          // a0 = signum
                    cx.x[REG_A1] = siginfo_ptr;     // a1 = &siginfo (on stack)
                    cx.x[REG_A2] = ucontext_ptr;    // a2 = &ucontext (on stack)
                    cx.x[1] = signal_restorer;      // ra = sa_restorer
                    cx.x[REG_SP] = new_sp;
                    // 记录 handler 入口 sp：handler 若经 siglongjmp 退出（不走
                    // sigreturn），用户 sp 会回退到此值之上，据此检测 stale 上下文。
                    if let Some(process) = t.get_mut(pid) {
                        process.signal_handler_sp = new_sp;
                    }

                    // 将被中断的上下文写入用户栈上的 ucontext，
                    // 以便 handler 能读取/修改 uc_sigmask 和 uc_mcontext
                    let (saved_ctx, old_mask) = {
                        let p = t.get(pid).unwrap();
                        let ctx = p.saved_trap_context.clone().unwrap();
                        let mask = p.saved_signal_mask.unwrap_or(0);
                        (ctx, mask)
                    };
                    #[cfg(target_arch = "loongarch64")]
                    let saved_fpu = crate::arch::loongarch64::fpu::snapshot_for_signal(pid);
                    let as_pid = if t.get(pid).map(|p| p.mm.is_some()).unwrap_or(false) {
                        pid
                    } else {
                        tgid
                    };
                    // 无 SA_RESTORER 时优先用共享 trampoline 页（见
                    // SIGRETURN_TRAMPOLINE_VA），仅当固定地址被占用才退回栈路径。
                    let signal_restorer = if signal_restorer == trampoline_ptr {
                        let vdso_va = t
                            .get(as_pid)
                            .and_then(|p| p.mm.as_ref())
                            .and_then(|mm| ensure_sigreturn_page(&mut *mm.lock()));
                        match vdso_va {
                            Some(va) => {
                                cx.x[1] = va; // ra = 共享 trampoline
                                va
                            }
                            None => trampoline_ptr,
                        }
                    } else {
                        signal_restorer
                    };
                    let frame_ready = if let Some(target) = t.get(as_pid) {
                        if let Some(ref mm) = target.mm {
                            let mut addr_space = mm.lock();
                            fault_in_user_write_range(&mut *addr_space, new_sp, frame_size)
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if !frame_ready {
                        if crate::cmdline::execdbg_enabled() {
                            crate::println!(
                                "[EXECDBG] sigframe-fail pid={} as_pid={} signum={} new_sp={:#x} size={}",
                                pid, as_pid, signum, new_sp, frame_size
                            );
                        }
                        drop(table);
                        // 信号 frame 准备失败 → 强制 SIGSEGV(11) core dump 退出。
                        crate::kernel::coredump::do_coredump(11);
                        crate::sched::process::exit_group_with_reason(
                            crate::sched::process::ExitReason::signaled_with_dump(11),
                        );
                    }
                    if crate::cmdline::sigfrdbg_enabled() {
                        if let Some(target) = t.get(as_pid) {
                            if let Some(ref mm) = target.mm {
                                let addr_space = mm.lock();
                                sigfrdbg_dump_delivery(
                                    pid,
                                    signum,
                                    handler_addr,
                                    sp,
                                    new_sp,
                                    siginfo_ptr,
                                    ucontext_ptr,
                                    signal_restorer,
                                    &saved_ctx,
                                    &*addr_space,
                                );
                            }
                        }
                    }

                    if let Some(target) = t.get(as_pid) {
                        if let Some(ref mm) = target.mm {
                            let mut addr_space = mm.lock();

                            // Slice 2.0：优先用 rt_sigqueueinfo 入队的真实
                            // siginfo_t payload（整 128B），并强制写 +0 偏移
                            // 为 signum（防御 sender 伪造 si_signo）。signalfd
                            // 路径（siginfo_fd）若同时存在，仍在 +24 处覆盖
                            // si_status/sigfd（保持原有 signalfd 兼容性）。
                            if let Some(ref qbuf) = queued_siginfo {
                                copy_siginfo_to_user(&mut *addr_space, siginfo_ptr, qbuf);
                                write_u64_to_user_as(&mut *addr_space, siginfo_ptr, signum as u64);
                                if let Some(fd) = siginfo_fd {
                                    write_u64_to_user_as(&mut *addr_space, siginfo_ptr + 24, fd as u32 as u64);
                                }
                            } else {
                                // siginfo 头 4 个 u64 连续且 16 字节对齐同页，
                                // 批量一次页表翻译写入，替代 4 次独立 translate。
                                let head = [
                                    signum as u64,
                                    0,
                                    0,
                                    siginfo_fd.map(|fd| fd as u32 as u64).unwrap_or(0),
                                ];
                                write_u64_batch_to_user(&mut *addr_space, siginfo_ptr, &head);
                            }
                            // uc_sigmask at ucontext + 0x28
                            write_u64_to_user_as(&mut *addr_space, ucontext_ptr + 0x28, old_mask);
                            // uc_mcontext.sc_regs: [0]=pc, [1]=x1, ..., [31]=x31
                            // saved_ctx[0]=sepc, saved_ctx[i]=x[i]
                            if signal_restorer == trampoline_ptr {
                                write_u64_to_user_as(&mut *addr_space, trampoline_ptr, RT_SIGRETURN_TRAMPOLINE);
                                let trampoline_vpn = VirtAddr(trampoline_ptr).floor();
                                if let Some(ppn) = addr_space.page_table().translate(trampoline_vpn) {
                                    if let Some(flags) = addr_space.page_table().get_pte_flags(trampoline_vpn) {
                                        // 栈页已带 X（同页重复投递）时跳过 remap+
                                        // TLB 冲刷 + fence.i（fence.i 在 TCG 下清
                                        // 整个翻译缓存，代价极高）。
                                        if !flags.contains(PTEFlags::X) {
                                            addr_space.page_table_mut().remap(trampoline_vpn, ppn, flags | PTEFlags::X);
                                            crate::arch::asm_helpers::flush_tlb_va(trampoline_vpn.0 << 12);
                                            crate::arch::fence_i();
                                        }
                                    }
                                }
                            }
                            // Batch-write registers — 1-2 page translations instead of 32.
                            #[cfg(target_arch = "loongarch64")]
                            {
                                let mut buf = [0u64; 33];
                                buf[0] = saved_ctx[0] as u64; // pc
                                buf[1] = 0; // x0 = zero
                                for i in 1..32usize {
                                    buf[i + 1] = saved_ctx[i] as u64;
                                }
                                write_u64_batch_to_user(&mut *addr_space, ucontext_ptr + 0xb0, &buf);
                                write_loongarch_fpu_signal_frame(
                                    &mut *addr_space,
                                    ucontext_ptr,
                                    &saved_fpu,
                                );
                            }
                            #[cfg(not(target_arch = "loongarch64"))]
                            {
                                let mut buf = [0u64; 32];
                                for i in 0..32usize {
                                    buf[i] = saved_ctx[i] as u64;
                                }
                                write_u64_batch_to_user(&mut *addr_space, ucontext_ptr + 0xb0, &buf);
                            }
                        }
                    }
                    if crate::cmdline::execdbg_enabled() {
                        const SYNC_MASK: u64 = (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 10);
                        if sig_bit & SYNC_MASK != 0 && sigdbg_budget_ok() {
                            // 关键不变量：这里改的 `cx` 必须就是 trap 返回用户态
                            // 时真正恢复的那份上下文，即 PCB 的 `trap_context_ptr`。
                            // 若两者地址不同，信号帧的 pc/sp 修改会被整份丢弃，
                            // 用户态从原 PC 原地重抛同一异常（观测：同一 ERA 反复
                            // 故障、每次 saved_ctx 又变回 false 的无限循环）。
                            let cx_addr = cx as *const _ as usize;
                            let tcp = t.get(pid).map_or(0, |p| p.trap_context_ptr);
                            crate::println!(
                                "[EXECDBG] sigframe-ok pid={} signum={} pc={:#x} sp={:#x} ra={:#x} old_pc={:#x} mask={:#x} cx={:#x} tcp={:#x} same={}",
                                pid, signum, cx.sepc, cx.x[REG_SP], cx.x[1], saved_ctx[0],
                                t.get(pid).map_or(0, |p| p.signal_mask),
                                cx_addr, tcp, cx_addr == tcp
                            );
                        }
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
pub(crate) fn check_and_deliver_fd_signals(cx: &mut crate::arch::TrapContext) {
    let should_deliver = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        match table.as_ref().and_then(|t| t.current.and_then(|pid| t.get(pid))) {
            Some(process) => {
                let deliverable = process.pending_signals & !process.signal_mask;
                if deliverable == 0 {
                    false
                } else {
                    let sig_bit = deliverable & deliverable.wrapping_neg();
                    let signum = sig_bit.trailing_zeros() as i32 + 1;
                    process.pending_signal_fds.iter().any(|(sig, _)| *sig == signum)
                }
            }
            None => false,
        }
    };
    if should_deliver {
        check_and_deliver_signals(cx);
    }
}

/// rt_sigsuspend(133) — 临时替换信号掩码并挂起，直到收到信号
///
// Re-export syscall handlers for backward compatibility
pub(crate) use super::signal_syscall::{
    sys_rt_sigsuspend, sys_rt_sigaction, sys_rt_sigprocmask,
    sys_rt_sigpending, sys_rt_sigtimedwait,
    sys_kill, sys_sigreturn, sys_tkill, sys_tgkill,
    sys_rt_sigqueueinfo, sys_rt_tgsigqueueinfo,
    sys_sigaltstack, deliver_mq_notify_signal,
    copy_siginfo_from_user, enqueue_pending_siginfo_locked,
    enqueue_sigsys_for_current,
};
// Import shared helpers defined in signal_syscall
use super::signal_syscall::{
    sigfrdbg_dump_delivery, sigfrdbg_dump_words,
    write_u64_to_user_as, write_u64_batch_to_user, copy_siginfo_to_user,
};
