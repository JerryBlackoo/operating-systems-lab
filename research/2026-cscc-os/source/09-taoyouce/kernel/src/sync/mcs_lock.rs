//! MCS 队列自旋锁（Linux `qspinlock` 的 MCS 等待结构等价实现）
//!
//! 与 ticket 锁的区别：
//! - 等待者在**自己的 per-CPU 节点**上自旋，每个 hart 只读自己的
//!   cacheline；释放者只写下一节点的 `locked`，不广播推进一个共享 owner；
//! - 取号入队只有一次 `tail.swap`，是 cacheline 争用最小的排队自旋锁；
//! - 必须关本地中断：节点按 hart 复用，同 hart 重入会破坏队列。
//!
//! 本项目用它替换最大全局锁 `PROCESS_TABLE` 的 ticket 实现。其余全局内存
//! 锁继续用 ticket（它们不保证关中断，使用 MCS 节点不安全）。

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use crate::percpu::{current_cpu_id, MAX_CPUS};

pub static MCS_CONTENDED: AtomicUsize = AtomicUsize::new(0);
pub static MCS_SPIN_ROUNDS: AtomicUsize = AtomicUsize::new(0);

/// per-CPU MCS 节点，cacheline 对齐避免相邻 hart 节点伪共享。
#[repr(align(64))]
struct McsNode {
    next: AtomicPtr<McsNode>,
    locked: AtomicBool,
}

impl McsNode {
    const fn new() -> Self {
        Self {
            next: AtomicPtr::new(core::ptr::null_mut()),
            locked: AtomicBool::new(false),
        }
    }
}

static MCS_NODES: [McsNode; MAX_CPUS] = [const { McsNode::new() }; MAX_CPUS];

/// 关中断 + MCS 队列锁。
pub struct IrqMcsSpinLock<T> {
    tail: AtomicPtr<McsNode>,
    data: UnsafeCell<T>,
    holder_ra: AtomicUsize,
    holder_pid: AtomicUsize,
}

pub struct IrqMcsSpinLockGuard<'a, T> {
    lock: &'a IrqMcsSpinLock<T>,
    node: *mut McsNode,
    was_enabled: bool,
}

unsafe impl<T: Send> Sync for IrqMcsSpinLock<T> {}
unsafe impl<T: Send> Send for IrqMcsSpinLock<T> {}

impl<T> IrqMcsSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            tail: AtomicPtr::new(core::ptr::null_mut()),
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

    #[inline]
    pub fn lock(&self) -> IrqMcsSpinLockGuard<'_, T> {
        let was_enabled = crate::arch::is_interrupt_enabled();
        crate::arch::disable_interrupts();

        let cpu = current_cpu_id();
        let node: *mut McsNode = &MCS_NODES[cpu] as *const McsNode as *mut McsNode;
        unsafe {
            (*node).next.store(core::ptr::null_mut(), Ordering::Relaxed);
            (*node).locked.store(true, Ordering::Relaxed);
        }

        let prev = self.tail.swap(node, Ordering::AcqRel);
        let mut rounds: u64 = 0;
        let mut reported = false;
        if !prev.is_null() {
            unsafe {
                (*prev).next.store(node, Ordering::Release);
            }
            while unsafe { (*node).locked.load(Ordering::Acquire) } {
                core::hint::spin_loop();
                rounds += 1;
                if rounds & 0xff == 0 {
                    crate::mm::tlb::complete_current_pending_flush();
                }
                if rounds >= (1 << 22) && !reported {
                    reported = true;
                    if crate::cmdline::spindbg_enabled() {
                        let (hra, hpid) = self.holder();
                        crate::println!(
                            "[MCSSPIN] lock={:#x} held_by_ra={:#x} held_by_pid={} waiter_ra={:#x} waiter_pid={} rounds={}",
                            self as *const Self as usize,
                            hra,
                            hpid,
                            return_address(),
                            current_pid_hint(),
                            rounds
                        );
                    }
                }
            }
        }
        if rounds != 0 {
            MCS_CONTENDED.fetch_add(1, Ordering::Relaxed);
            MCS_SPIN_ROUNDS.fetch_add(rounds as usize, Ordering::Relaxed);
        }
        self.holder_ra.store(return_address(), Ordering::Relaxed);
        self.holder_pid.store(current_pid_hint(), Ordering::Relaxed);
        IrqMcsSpinLockGuard {
            lock: self,
            node,
            was_enabled,
        }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<IrqMcsSpinLockGuard<'_, T>> {
        let was_enabled = crate::arch::is_interrupt_enabled();
        crate::arch::disable_interrupts();
        if !self.tail.load(Ordering::Acquire).is_null() {
            if was_enabled {
                crate::arch::enable_interrupts();
            }
            return None;
        }
        let cpu = current_cpu_id();
        let node: *mut McsNode = &MCS_NODES[cpu] as *const McsNode as *mut McsNode;
        unsafe {
            (*node).next.store(core::ptr::null_mut(), Ordering::Relaxed);
            (*node).locked.store(true, Ordering::Relaxed);
        }
        let ok = self
            .tail
            .compare_exchange(
                core::ptr::null_mut(),
                node,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if !ok {
            if was_enabled {
                crate::arch::enable_interrupts();
            }
            return None;
        }
        self.holder_ra.store(return_address(), Ordering::Relaxed);
        self.holder_pid.store(current_pid_hint(), Ordering::Relaxed);
        Some(IrqMcsSpinLockGuard {
            lock: self,
            node,
            was_enabled,
        })
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        !self.tail.load(Ordering::Acquire).is_null()
    }
}

impl<'a, T> Deref for IrqMcsSpinLockGuard<'a, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> DerefMut for IrqMcsSpinLockGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqMcsSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.holder_ra.store(0, Ordering::Relaxed);
        self.lock.holder_pid.store(0, Ordering::Relaxed);
        let node = self.node;
        unsafe {
            let mut next = (*node).next.load(Ordering::Acquire);
            if next.is_null() {
                if self
                    .lock
                    .tail
                    .compare_exchange(
                        node,
                        core::ptr::null_mut(),
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    if self.was_enabled {
                        crate::arch::enable_interrupts();
                    }
                    return;
                }
                // 有新等待者已换入 tail，但可能还没把它链到本节点；等它发布。
                while next.is_null() {
                    core::hint::spin_loop();
                    next = (*node).next.load(Ordering::Acquire);
                }
            }
            (*next).locked.store(false, Ordering::Release);
        }
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
