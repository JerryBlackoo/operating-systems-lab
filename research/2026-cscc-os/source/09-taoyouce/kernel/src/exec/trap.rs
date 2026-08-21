use crate::loader::ElfInfo;
use crate::sched::process::{ProcessControlBlock, PROCESS_TABLE};

pub(super) fn reset_for_exec(
    process: &mut ProcessControlBlock,
    elf_info: &ElfInfo,
    argc: usize,
    envc: usize,
) {
    let ksp = process.kernel_stack_top();
    unsafe {
        if let Some(trap_cx) = process.trap_context_mut() {
            for i in 0..32 {
                trap_cx.x[i] = 0;
            }
            trap_cx.sepc = elf_info.entry;
            #[cfg(target_arch = "loongarch64")]
            { trap_cx.sstatus = 3 | (1 << 2); }
            #[cfg(target_arch = "riscv64")]
            {
                trap_cx.sstatus = (1 << 5);
            }
            trap_cx.set_sp(elf_info.stack_top);
            trap_cx.kernel_sp = ksp;
            let word = core::mem::size_of::<usize>();
            #[cfg(target_arch = "riscv64")]
            {
                trap_cx.x[10] = 0;
                trap_cx.x[11] = elf_info.stack_top + word;
                trap_cx.x[12] = elf_info.stack_top + word * (1 + argc + 1);
                trap_cx.x[13] = trap_cx.x[12] + word * (envc + 1);
            }
            #[cfg(target_arch = "loongarch64")]
            {
                trap_cx.x[4] = 0;
                trap_cx.x[5] = elf_info.stack_top + word;
                trap_cx.x[6] = elf_info.stack_top + word * (1 + argc + 1);
                trap_cx.x[7] = trap_cx.x[6] + word * (envc + 1);
            }
        } else {
        }
    }
}

pub(super) fn return_to_user_after_exec() -> ! {
    use crate::arch::usertrapret;
    use crate::arch::TrapContext;

    let (trap_cx_ptr, user_satp) = {
        let mut table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_mut().expect("Process table not initialized");
        let current_pid = table.current.expect("No current process");
        let process = table.get_mut(current_pid).expect("Current process not found");

        let user_satp = process.satp();
        let trap_cx = unsafe {
            process.trap_context_mut().expect("Process has no TrapContext")
        };
        let trap_cx_ptr = trap_cx as *mut TrapContext;

        (trap_cx_ptr, user_satp)
    };

    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence.i", options(nostack, preserves_flags));
    }

    unsafe {
        usertrapret(&mut *trap_cx_ptr, user_satp);
    }
}
