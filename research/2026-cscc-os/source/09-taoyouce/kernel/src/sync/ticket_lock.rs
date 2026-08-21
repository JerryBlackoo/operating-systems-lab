//! FIFO ticket 自旋锁（对标 Linux `ticket_spinlock` / `qspinlock` 的公平性目标）
//!
//! 与 `SpinLock`（TTAS）的区别：
//! - 每个等待者在入口只做**一次** `fetch_add` 取号，之后全部是本地寄存器
//!   与共享 `owner` 的纯读自旋，不再反复 CAS 写争用；
//! - 严格按取号顺序获得锁（FIFO），消除 TTAS 在高争用下的饥饿与
//!   cacheline ping-pong；
//! - 和 `SpinLock` 一样，在自旋期间主动应答本 hart 的 TLB shootdown，
//!   避免「等待者关中断等锁 / 持锁者等 TLB ack」的跨核互等。
//!
//! `IrqTicketSpinLock` 在 ticket 之上叠加关本地中断，用于 per-CPU 数据：
//! 同一 hart 的定时器/设备中断可能在持锁期间重入同一把 per-CPU 锁。

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};

/// ticket 锁争用统计：`contended` 是取号后发现必须等待的加锁次数，
/// `spin_rounds` 是等待者累计自旋轮数。
pub static TICKET_CONTENDED: AtomicUsize = AtomicUsize::new(0);
pub static TICKET_SPIN_ROUNDS: AtomicUsize = AtomicUsize::new(0);

/// FIFO ticket 自旋锁。
pub struct TicketSpinLock<T> {
    /// 下一个可领取的 ticket 号。
    next: AtomicUsize,
    /// 当前允许进入临界区的 ticket 号。
    owner: AtomicUsize,
    data: UnsafeCell<T>,
    /// 诊断：最近一次成功持锁者的返回地址 / pid。
    holder_ra: AtomicUsize,
    holder_pid: AtomicUsize,
}

pub struct TicketSpinLockGuard<'a, T> {
    lock: &'a TicketSpinLock<T>,
    /// 已被 `finish()` 手动解锁（防止 IrqTicket 包装器二次释放）。
    finished: bool,
}

unsafe impl<T: Send> Sync for TicketSpinLock<T> {}
unsafe impl<T: Send> Send for TicketSpinLock<T> {}

impl<T> TicketSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            next: AtomicUsize::new(0),
            owner: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
            holder_ra: AtomicUsize::new(0),
            holder_pid: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn holder(&self) -> (usize, usize) {
        (
            self.holder_ra.load(Ordering::Relaxed),
            self.holder_pid.load(Ordering::Relaxed),
        )
    }

    /// 获取锁：取号后等待 `owner == my_ticket`。
    #[inline]
    pub fn lock(&self) -> TicketSpinLockGuard<'_, T> {
        let caller_ra = return_address();
        let my_ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let mut rounds: u64 = 0;
        let mut reported = false;
        let mut counted = false;
        loop {
            let now_serving = self.owner.load(Ordering::Acquire);
            if now_serving == my_ticket {
                self.holder_ra.store(caller_ra, Ordering::Relaxed);
                self.holder_pid.store(current_pid_hint(), Ordering::Relaxed);
                if counted {
                    TICKET_SPIN_ROUNDS.fetch_add(rounds as usize, Ordering::Relaxed);
                }
                return TicketSpinLockGuard {
                    lock: self,
                    finished: false,
                };
            }
            if !counted {
                counted = true;
                TICKET_CONTENDED.fetch_add(1, Ordering::Relaxed);
            }
            rounds += 1;
            core::hint::spin_loop();
            // 同 SpinLock：等待期间继续应答 TLB shootdown。
            if rounds & 0xff == 0 {
                crate::mm::tlb::complete_current_pending_flush();
            }
            if rounds >= (1 << 22) && !reported {
                reported = true;
                if crate::cmdline::spindbg_enabled() {
                    let (hra, hpid) = self.holder();
                    crate::println!(
                        "[TICKETSPIN] lock={:#x} held_by_ra={:#x} held_by_pid={} waiter_ra={:#x} waiter_pid={} ticket={} serving={} rounds={}",
                        self as *const Self as usize,
                        hra, hpid, caller_ra, current_pid_hint(),
                        my_ticket, self.owner.load(Ordering::Relaxed), rounds
                    );
                }
            }
        }
    }

    /// 非阻塞获取；仅当锁空闲且无等待者时成功。
    #[inline]
    pub fn try_lock(&self) -> Option<TicketSpinLockGuard<'_, T>> {
        loop {
            let serving = self.owner.load(Ordering::Relaxed);
            let next = self.next.load(Ordering::Relaxed);
            if serving != next {
                return None;
            }
            // 只有 next==owner（无等待者）时才能安全 CAS 领号；失败说明
            // 有并发竞争者，回到循环重新观察。
            if self
                .next
                .compare_exchange(
                    serving,
                    serving.wrapping_add(1),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                self.holder_ra.store(return_address(), Ordering::Relaxed);
                self.holder_pid.store(current_pid_hint(), Ordering::Relaxed);
                return Some(TicketSpinLockGuard {
                    lock: self,
                    finished: false,
                });
            }
        }
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        self.next.load(Ordering::Relaxed) != self.owner.load(Ordering::Relaxed)
    }
}

impl<T> TicketSpinLockGuard<'_, T> {
    /// 手动完成解锁。Drop 会检查该标记，保证恰好解锁一次。
    #[inline]
    pub(super) fn finish(&mut self) {
        if self.finished {
            return;
        }
        // 先发布临界区写，再推进服务号。
        self.lock.holder_ra.store(0, Ordering::Relaxed);
        self.lock.holder_pid.store(0, Ordering::Relaxed);
        let serving = self.lock.owner.load(Ordering::Relaxed);
        self.lock
            .owner
            .store(serving.wrapping_add(1), Ordering::Release);
        self.finished = true;
    }
}

impl<T> Drop for TicketSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.finish();
    }
}

impl<'a, T> Deref for TicketSpinLockGuard<'a, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> DerefMut for TicketSpinLockGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

/// 关中断 + FIFO ticket 锁，用于同一 hart 可重入路径上的 per-CPU 数据。
pub struct IrqTicketSpinLock<T> {
    inner: TicketSpinLock<T>,
}

pub struct IrqTicketSpinLockGuard<'a, T> {
    inner: TicketSpinLockGuard<'a, T>,
    was_enabled: bool,
}

unsafe impl<T: Send> Sync for IrqTicketSpinLock<T> {}
unsafe impl<T: Send> Send for IrqTicketSpinLock<T> {}

impl<T> IrqTicketSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: TicketSpinLock::new(data),
        }
    }

    /// 关本地中断后取 ticket；释放时先解锁再恢复原中断状态。
    ///
    /// 约束同 `IrqSpinLock`：临界区内不得睡眠/调度。
    #[inline]
    pub fn lock(&self) -> IrqTicketSpinLockGuard<'_, T> {
        let was_enabled = crate::arch::is_interrupt_enabled();
        crate::arch::disable_interrupts();
        IrqTicketSpinLockGuard {
            inner: self.inner.lock(),
            was_enabled,
        }
    }

    /// 非阻塞版本：拿不到锁会恢复被本函数关闭的中断。
    #[inline]
    pub fn try_lock(&self) -> Option<IrqTicketSpinLockGuard<'_, T>> {
        let was_enabled = crate::arch::is_interrupt_enabled();
        crate::arch::disable_interrupts();
        match self.inner.try_lock() {
            Some(inner) => Some(IrqTicketSpinLockGuard { inner, was_enabled }),
            None => {
                if was_enabled {
                    crate::arch::enable_interrupts();
                }
                None
            }
        }
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }
}

impl<'a, T> Deref for IrqTicketSpinLockGuard<'a, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<'a, T> DerefMut for IrqTicketSpinLockGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> Drop for IrqTicketSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        // 先解锁，再恢复中断（同 IrqSpinLock 顺序）。
        self.inner.finish();
        if self.was_enabled {
            crate::arch::enable_interrupts();
        }
    }
}

#[inline(always)]
fn return_address() -> usize {
    let ra: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("mv {}, ra", out(reg) ra);
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("move {}, $ra", out(reg) ra);
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        ra = 0;
    }
    ra
}

#[inline(always)]
fn current_pid_hint() -> usize {
    crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed)
}
