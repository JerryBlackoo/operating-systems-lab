//! Reference-counted address space wrapper (对标 Linux `struct mm_struct`)
//!
//! 将 `AddressSpace` 包装为引用计数结构，用 `Arc<MmStruct>` 在进程间共享：
//!
//! - **`mm_users`** (`AtomicU32`)：活跃用户数。对标 Linux `mm->mm_users`。
//!   - `fork()` 带 `CLONE_VM` 时 +1（子线程共享同一地址空间）
//!   - 线程 `exit()` 时 -1
//!   - 减到 0 时释放用户页（`release_user_frames`）并调 `mmdrop`
//!
//! - **`mm_count`** (`AtomicU32`)：底层生命周期引用数。对标 Linux `mm->mm_count`。
//!   - `mm_users > 0` 贡献一次 +1（`mmget` 首次时隐含）
//!   - 内核临时引用（如 `/proc/pid/maps` 读取、GUP pin）也 +1
//!   - 减到 0 时 drop 整个 MmStruct（释放页表结构页等）
//!
//! # 对比旧设计
//!
//! 旧：`ProcessControlBlock.address_space: Option<AddressSpace>` 直接嵌入 PCB，
//! `CLONE_VM` 用 `shared_vm_parent: Option<Pid>` hack，退出时 O(N) 遍历推导所有者。
//!
//! 新：`ProcessControlBlock.mm: Option<Arc<MmStruct>>`，`CLONE_VM` = `Arc::clone`，
//! 退出时 `mmput()` 原子递减，O(1) 确定是否最后一个用户，零遍历。

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::sync::SpinLock;
use super::address_space::AddressSpace;

/// 引用计数地址空间（对标 Linux `struct mm_struct`）。
///
/// 通过 `Arc<MmStruct>` 在进程间共享。内部用 SpinLock 保护 AddressSpace
/// 的可变访问（对标 Linux `mm->mmap_lock` / `mm->page_table_lock`）。
pub struct MmStruct {
    /// 受保护的地址空间
    inner: SpinLock<AddressSpace>,

    /// 活跃用户数（线程/vfork 共享者）。
    ///
    /// - fork(CLONE_VM)：+1
    /// - exit/execve：-1
    /// - 降到 0 → release_user_frames + mmdrop
    mm_users: AtomicU32,

    /// 底层生命周期引用数。
    ///
    /// - mm_users > 0 时贡献 1
    /// - 内核临时引用（GUP、/proc）各 +1
    /// - 降到 0 → drop MmStruct
    mm_count: AtomicU32,
}

// SAFETY: MmStruct 内部 AddressSpace 通过 SpinLock 保护，原子字段本身 Sync。
unsafe impl Sync for MmStruct {}
unsafe impl Send for MmStruct {}

impl MmStruct {
    /// 从现有 AddressSpace 创建 MmStruct（mm_users=1, mm_count=1）。
    ///
    /// 用于：
    /// - `create_address_space`（新进程创建地址空间）
    /// - `fork`（非 CLONE_VM，深拷贝 CoW 后包装新 AddressSpace）
    pub fn new(address_space: AddressSpace) -> Self {
        Self {
            inner: SpinLock::new(address_space),
            mm_users: AtomicU32::new(1),
            mm_count: AtomicU32::new(1),
        }
    }

    /// 获取 AddressSpace 的互斥访问权。
    ///
    /// 对标 Linux `mmap_write_lock(mm)` / `spin_lock(&mm->page_table_lock)`。
    /// 调用方通过 SpinLockGuard 的 Deref/DerefMut 访问 AddressSpace。
    #[inline]
    pub fn lock(&self) -> crate::sync::SpinLockGuard<'_, AddressSpace> {
        self.inner.lock()
    }

    /// 尝试获取 AddressSpace 的互斥访问权（非阻塞）。
    #[inline]
    pub fn try_lock(&self) -> Option<crate::sync::SpinLockGuard<'_, AddressSpace>> {
        self.inner.try_lock()
    }

    // ── mm_users 操作 ──────────────────────────────────────────────

    /// 增加活跃用户数（CLONE_VM / vfork 时调用）。
    ///
    /// 对标 Linux `mmget(mm)`。
    ///
    /// # Panics
    /// mm_users 已经为 0 时 panic（说明地址空间已释放，不应再获取）。
    pub fn mmget(&self) {
        let prev = self.mm_users.fetch_add(1, Ordering::AcqRel);
        debug_assert!(
            prev > 0,
            "[MmStruct] mmget: mm_users was 0 — use mmget_not_zero for safe check",
        );
    }

    /// 安全版本的 mmget：仅当 mm_users > 0 时成功（+1 并返回 true）。
    ///
    /// 对标 Linux `mmget_not_zero(mm)`。用于内核临时访问（如 /proc/pid/maps）
    /// 需要安全检查地址空间是否仍有活跃用户。
    pub fn mmget_not_zero(&self) -> bool {
        let mut current = self.mm_users.load(Ordering::Acquire);
        loop {
            if current == 0 {
                return false;
            }
            match self.mm_users.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// 减少活跃用户数。
    ///
    /// 对标 Linux `mmput(mm)`。
    ///
    /// - mm_users 从 N → N-1（N > 1）：仅递减
    /// - mm_users 从 1 → 0：释放用户帧（`release_user_frames`），然后调 `mmdrop`
    ///
    /// 返回 `true` 表示调用方是最后一个用户（已触发 release + mmdrop）。
    pub fn mmput(&self) -> bool {
        let prev = self.mm_users.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "[MmStruct] mmput: mm_users was already 0");
        if prev == 1 {
            // 最后一个活跃用户退出：释放用户页
            self.lock().release_user_frames();
            // 递减底层引用（mm_users > 0 贡献的那一次）
            self.mmdrop();
            true
        } else {
            false
        }
    }

    /// 获取当前 mm_users 值（调试/统计用）。
    #[inline]
    pub fn users(&self) -> u32 {
        self.mm_users.load(Ordering::Relaxed)
    }

    // ── mm_count 操作 ──────────────────────────────────────────────

    /// 增加底层引用数。
    ///
    /// 对标 Linux `mmgrab(mm)`。用于内核临时引用（GUP pin、/proc 读取等）。
    pub fn mmgrab(&self) {
        let prev = self.mm_count.fetch_add(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "[MmStruct] mmgrab: mm_count was 0");
    }

    /// 减少底层引用数。
    ///
    /// 对标 Linux `mmdrop(mm)`。mm_count 降到 0 时，MmStruct 应被 drop
    /// （释放页表结构页等）。由于 MmStruct 通过 Arc 管理，mm_count 降到 0 时
    /// 不直接 drop self，而是标记为可回收状态——实际 drop 由最后一个 Arc
    /// 引用的 Drop 触发。
    ///
    /// 返回 `true` 表示 mm_count 降到了 0。
    pub fn mmdrop(&self) -> bool {
        let prev = self.mm_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "[MmStruct] mmdrop: mm_count was already 0");
        prev == 1
    }

    /// 获取当前 mm_count 值（调试/统计用）。
    #[inline]
    pub fn count(&self) -> u32 {
        self.mm_count.load(Ordering::Relaxed)
    }
}

// ── Arc<MmStruct> 辅助函数 ─────────────────────────────────────────────

/// 对 Arc<MmStruct> 执行 mmput 并在 mm_users 降到 0 时允许 Arc 回收。
///
/// 这是进程退出时的标准调用路径：
/// 1. `mmput` 减 mm_users
/// 2. 若 mm_users → 0：release_user_frames + mmdrop（减 mm_count）
/// 3. 若 mm_count → 0 且这是最后一个 Arc：MmStruct 被 drop（释放页表）
///
/// 调用方应在 mmput_arc 后 drop 自己的 Arc<MmStruct>（通常通过 `.take()`）。
pub fn mmput_arc(mm: &Arc<MmStruct>) {
    mm.mmput();
}

/// 为 CLONE_VM 创建共享引用：clone Arc + mmget。
///
/// 返回子进程应持有的 Arc<MmStruct>。
pub fn mmget_clone(mm: &Arc<MmStruct>) -> Arc<MmStruct> {
    mm.mmget();
    mm.clone()
}

impl Drop for MmStruct {
    fn drop(&mut self) {
        // AddressSpace 的 Drop 会释放页表结构页等资源。
        // 用户页应该已在 mmput → release_user_frames 中释放。
        // 这里只需确认状态一致性。
        debug_assert!(
            self.mm_users.load(Ordering::Relaxed) == 0,
            "[MmStruct] Drop: mm_users != 0 — user frames may not have been released",
        );
    }
}
