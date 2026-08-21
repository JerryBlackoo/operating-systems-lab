//! 同步原语
//!
//! - `SpinLock`：TTAS 自旋锁（通用共享状态）
//! - `IrqSpinLock`：关中断 TTAS（中断可重入路径）
//! - `TicketSpinLock` / `IrqTicketSpinLock`：FIFO ticket 公平锁（热路径/
//!   per-CPU 数据），自旋中同样应答 TLB shootdown

#![allow(dead_code)]

mod spinlock;
mod ticket_lock;
mod local_irq;
mod mcs_lock;

pub use spinlock::{IrqSpinLock, IrqSpinLockGuard, SpinLock, SpinLockGuard};
pub use ticket_lock::{IrqTicketSpinLock, IrqTicketSpinLockGuard, TicketSpinLock, TicketSpinLockGuard};
pub use ticket_lock::{TICKET_CONTENDED, TICKET_SPIN_ROUNDS};
pub use local_irq::{LocalIrqCell, LocalIrqGuard};
pub use mcs_lock::{IrqMcsSpinLock, IrqMcsSpinLockGuard, MCS_CONTENDED, MCS_SPIN_ROUNDS};
pub use spinlock::{LOCK_CONTENDED, LOCK_PARKS, LOCK_SPIN_ROUNDS};

/// 关中断保护的临界区
pub struct InterruptGuard {
    /// 之前的中断状态
    was_enabled: bool,
}

impl InterruptGuard {
    /// 进入临界区（关中断）
    pub fn new() -> Self {
        let sstatus: usize;
        sstatus = crate::arch::asm_helpers::read_sstatus();
        let was_enabled = (sstatus >> 1) & 1 == 1;
        
        crate::arch::disable_interrupts();
        
        Self { was_enabled }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if self.was_enabled {
            crate::arch::enable_interrupts();
        }
    }
}
