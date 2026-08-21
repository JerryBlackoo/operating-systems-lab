//! 本 hart 独占数据 + 关本地中断（对标 Linux per-CPU 变量 + `local_irq_save`）
//!
//! Linux per-CPU 变量本身不需要原子锁：每个 CPU 只访问自己的副本，只需
//! 关本地中断防止同 hart 上中断上下文重入。本类型把这一语义做成类型：
//! - 零原子操作，比 `IrqSpinLock`/`IrqTicketSpinLock` 快；
//! - 调用方必须保证**绝不**跨 hart 访问同一 `LocalIrqCell`（这是 per-CPU
//!   数据的定义）；跨 hart 数据请使用 ticket/spin 锁。
//! - 临界区内不得重开中断或睡眠/调度。

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

pub struct LocalIrqCell<T> {
    data: UnsafeCell<T>,
}

pub struct LocalIrqGuard<'a, T> {
    cell: &'a LocalIrqCell<T>,
    was_enabled: bool,
}

unsafe impl<T: Send> Sync for LocalIrqCell<T> {}
unsafe impl<T: Send> Send for LocalIrqCell<T> {}

impl<T> LocalIrqCell<T> {
    pub const fn new(data: T) -> Self {
        Self {
            data: UnsafeCell::new(data),
        }
    }

    #[inline]
    pub fn lock(&self) -> LocalIrqGuard<'_, T> {
        let was_enabled = crate::arch::is_interrupt_enabled();
        crate::arch::disable_interrupts();
        LocalIrqGuard {
            cell: self,
            was_enabled,
        }
    }
}

impl<'a, T> Deref for LocalIrqGuard<'a, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*self.cell.data.get() }
    }
}

impl<'a, T> DerefMut for LocalIrqGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.cell.data.get() }
    }
}

impl<T> Drop for LocalIrqGuard<'_, T> {
    fn drop(&mut self) {
        if self.was_enabled {
            crate::arch::enable_interrupts();
        }
    }
}
