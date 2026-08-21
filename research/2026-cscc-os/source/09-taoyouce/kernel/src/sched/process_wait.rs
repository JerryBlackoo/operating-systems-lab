use crate::sched::process::{ExitReason, Pid};

#[derive(Debug, Clone, Copy)]
pub enum WaitidEvent {
    Exited(ExitReason),
    Stopped(i32),
    Continued,
}

pub(crate) fn wait_status_from_exit_reason(reason: ExitReason) -> i32 {
    match reason {
        ExitReason::Normal(code) => (code & 0xff) << 8,
        ExitReason::Signaled { sig, core_dumped } => {
            (sig & 0x7f) | (if core_dumped { 0x80 } else { 0 })
        }
    }
}

pub(crate) fn wait_status_from_stopped_signal(sig: i32) -> i32 {
    0x7f | ((sig & 0xff) << 8)
}

pub(crate) fn wait_status_from_continued() -> i32 {
    0xffff
}

pub(crate) fn build_waitid_siginfo(
    buf: &mut [u8; 128],
    child_pid: Pid,
    event: WaitidEvent,
    uid: u32,
    cpu_user_ticks: u64,
    cpu_kernel_ticks: u64,
) {
    const SIGCHLD: i32  = 17;
    const CLD_EXITED: i32 = 1;
    const CLD_KILLED: i32 = 2;
    const CLD_DUMPED: i32 = 3;
    const CLD_STOPPED: i32 = 5;
    const CLD_CONTINUED: i32 = 6;
    const SIGCONT: i32 = 18;

    let (si_code, si_status) = match event {
        WaitidEvent::Exited(reason) => match reason {
            ExitReason::Normal(code) => (CLD_EXITED, code),
            ExitReason::Signaled { sig, core_dumped } => {
                if core_dumped {
                    (CLD_DUMPED, sig & 0x7f)
                } else {
                    (CLD_KILLED, sig & 0x7f)
                }
            }
        },
        WaitidEvent::Stopped(sig) => (CLD_STOPPED, sig & 0xff),
        WaitidEvent::Continued => (CLD_CONTINUED, SIGCONT),
    };

    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    let scale = if freq >= 100 { freq / 100 } else { 1 };
    let si_utime = (cpu_user_ticks / scale) as i64;
    let si_stime = (cpu_kernel_ticks / scale) as i64;

    for b in buf.iter_mut() { *b = 0; }

    buf[0..4].copy_from_slice(&SIGCHLD.to_le_bytes());
    buf[8..12].copy_from_slice(&si_code.to_le_bytes());
    buf[16..20].copy_from_slice(&(child_pid as i32).to_le_bytes());
    buf[20..24].copy_from_slice(&uid.to_le_bytes());
    buf[24..28].copy_from_slice(&si_status.to_le_bytes());
    buf[32..40].copy_from_slice(&si_utime.to_le_bytes());
    buf[40..48].copy_from_slice(&si_stime.to_le_bytes());
}

/// 用户地址上界：覆盖程序/栈/mmap 全区（mmap 区可高于栈顶，见
/// syscall/memory.rs 的 USER_VA_MAX），拒绝内核/非规范地址。
const USER_VA_MAX: usize = 0x80_0000_0000;

#[inline]
fn is_valid_user_ptr(ptr: usize) -> bool {
    ptr != 0 && ptr < USER_VA_MAX
}

pub(crate) fn write_user_bytes(
    addr_space: &mut crate::mm::AddressSpace,
    user_va: usize,
    data: &[u8],
) -> bool {
    use crate::mm::{VirtAddr, PTEFlags};
    if user_va == 0 || !is_valid_user_ptr(user_va) {
        return false;
    }
    let mut written = 0usize;
    while written < data.len() {
        let va = VirtAddr(user_va + written);
        let vpn = va.floor();
        let page_offset = va.page_offset();
        if let Some(flags) = addr_space.page_table().get_pte_flags(vpn) {
            if !flags.contains(PTEFlags::W) {
                addr_space.handle_page_fault(va, true);
            }
        } else {
            addr_space.handle_page_fault(va, true);
        }
        let Some(ppn) = addr_space.page_table().translate(vpn) else { return false };
        let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + page_offset);
        let bytes_in_page = core::cmp::min(4096 - page_offset, data.len() - written);
        unsafe {
            core::ptr::copy_nonoverlapping(
                data[written..].as_ptr(),
                kva as *mut u8,
                bytes_in_page,
            );
        }
        written += bytes_in_page;
    }
    true
}
