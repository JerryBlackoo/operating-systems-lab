use crate::syscall::types::signal::SignalUserContext;
use crate::tasks::current_user_task;
use core::mem::size_of;
use executor::AsyncTask;
use libc_types::internal::SigAction;
use libc_types::signal::SignalNum;
use log::debug;
use polyhal_trap::trapframe::TrapFrameArgs;

use super::UserTaskContainer;

#[cfg(target_arch = "riscv64")]
const SIGNAL_RESTORER_CODE: [u8; 8] = [
    0x93, 0x08, 0xb0, 0x08, // li a7, 139
    0x73, 0x00, 0x00, 0x00, // ecall
];

#[cfg(target_arch = "loongarch64")]
const SIGNAL_RESTORER_CODE: [u8; 8] = [
    0x0b, 0x2c, 0x82, 0x03, // li.w $a7, 139
    0x00, 0x00, 0x2b, 0x00, // syscall 0
];

#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
const SIGNAL_RESTORER_CODE: [u8; 0] = [];

#[cfg(target_arch = "riscv64")]
const SA_RESTORER: usize = 0x0400_0000;

fn install_signal_restorer(sp: usize) -> usize {
    let restorer = sp + size_of::<SignalUserContext>();
    va!(restorer)
        .slice_mut_with_len(SIGNAL_RESTORER_CODE.len())
        .copy_from_slice(&SIGNAL_RESTORER_CODE);
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence.i");
    }
    restorer
}

impl UserTaskContainer {
    pub async fn handle_signal(&self, signal: SignalNum) {
        debug!(
            "handle signal: {:?} task_id: {}",
            signal,
            self.task.get_task_id()
        );

        // get the signal action for the signal.
        let sigaction = self.task.pcb.lock().sigaction[signal.num()].clone();

        if sigaction.handler == SigAction::SIG_IGN {
            // ignore signal if the handler of is SIG_IGN(1)
            return;
        } else if sigaction.handler == 0 || sigaction.handler == SigAction::SIG_DFL {
            // if there doesn't have signal handler.
            // Then use default handler. Exit or do nothing.
            if matches!(signal, SignalNum::CANCEL | SignalNum::SEGV | SignalNum::ILL) {
                current_user_task().exit_with_signal(signal.num());
            }
            return;
        }

        let cx_ref = self.task.force_cx_ref();
        // store task_mask and context.
        // alloc space for SignalUserContext at stack and align with 16 bytes.
        let sp = (cx_ref[TrapFrameArgs::SP]
            - size_of::<SignalUserContext>()
            - SIGNAL_RESTORER_CODE.len())
            & !0xF;
        let cx = unsafe { (sp as *mut SignalUserContext).as_mut().unwrap() };
        #[cfg(target_arch = "riscv64")]
        let restorer = if sigaction.flags & SA_RESTORER != 0 && sigaction.restorer > 1 {
            sigaction.restorer
        } else {
            install_signal_restorer(sp)
        };
        #[cfg(not(target_arch = "riscv64"))]
        let restorer = match sigaction.restorer {
            0 | 1 => install_signal_restorer(sp),
            restorer => restorer,
        };

        // change task context to do the signal.
        let mut tcb = self.task.tcb.write();
        cx.store_ctx(&cx_ref);
        cx.set_sig_mask(tcb.sigmask);

        tcb.sigmask = sigaction.mask;
        tcb.cx[TrapFrameArgs::SP] = sp;
        tcb.cx[TrapFrameArgs::SEPC] = sigaction.handler;
        tcb.cx[TrapFrameArgs::RA] = restorer;
        tcb.cx[TrapFrameArgs::ARG0] = signal.num();
        tcb.cx[TrapFrameArgs::ARG1] = 0;
        tcb.cx[TrapFrameArgs::ARG2] = cx as *mut SignalUserContext as usize;
        drop(tcb);
    }
}
