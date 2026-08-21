//! Get User Pages — 安全获取/pin 用户地址空间的物理帧 (对标 Linux mm/gup.c)
//!
//! 网络零拷贝、VirtIO DMA、`/proc/pid/mem` 读取等场景需要临时持有用户帧的
//! 物理地址，但帧可能在持有期间被 swap out / reclaim / CoW break。GUP 提供
//! 安全的获取机制：
//!
//! - `get_user_pages`：遍历页表查找用户帧，对每帧增加 MetaSlot ref_count，
//!   返回 PPN 列表。调用方用完后必须 `put_user_pages` 释放引用。
//!
//! - `pin_user_pages`：在 `get_user_pages` 基础上设置 PG_LOCKED flag，
//!   防止帧被回收。用于 DMA 等需要帧物理地址稳定的场景。
//!
//! # 当前简化
//!
//! RustMicroOS 暂无 swap，因此 GUP 的主要作用是：
//! 1. 通过 MetaSlot ref_count 增加引用，防止帧在 DMA 期间被 buddy 回收
//! 2. 统一替代各模块直接 `alloc_frame` + 手动 `dealloc_frame` 的裸 PPN 传递

use super::PhysPageNum;
use super::page_meta::{self, frame_flags};
use super::address_space::AddressSpace;

/// GUP 标志位。
pub struct GupFlags(u32);

impl GupFlags {
    /// 需要写权限
    pub const FOLL_WRITE: Self = Self(1 << 0);
    /// Pin 模式（设置 PG_LOCKED）
    pub const FOLL_PIN: Self = Self(1 << 1);

    pub fn contains(&self, flag: &Self) -> bool {
        self.0 & flag.0 != 0
    }

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// 从地址空间获取用户页的物理帧，并增加 MetaSlot 引用计数。
///
/// 遍历 [start_vpn, start_vpn + nr_pages) 的页表映射，对每个已映射帧的
/// MetaSlot ref_count +1。调用方用完后必须 `put_user_pages` 释放。
///
/// # 参数
/// - `address_space`：目标地址空间（需已锁定）
/// - `start_vpn`：起始虚拟页号
/// - `nr_pages`：需要获取的页数
/// - `flags`：GUP 标志（FOLL_WRITE / FOLL_PIN）
///
/// # 返回
/// - `Ok(ppns)`：成功获取的 PPN 列表（长度 == nr_pages）
/// - `Err(n)`：在第 n 页失败（前 n 页已获取的引用会被自动释放）
pub fn get_user_pages(
    address_space: &AddressSpace,
    start_vpn: super::VirtPageNum,
    nr_pages: usize,
    flags: GupFlags,
) -> Result<alloc::vec::Vec<PhysPageNum>, usize> {
    let mut ppns = alloc::vec::Vec::with_capacity(nr_pages);
    let pt = address_space.page_table();

    for i in 0..nr_pages {
        let vpn = super::VirtPageNum(start_vpn.0 + i);

        // 检查写权限（若请求 FOLL_WRITE，用 get_pte_flags 判断）
        if flags.contains(&GupFlags::FOLL_WRITE) {
            match pt.get_pte_flags(vpn) {
                Some(f) if f.contains(super::page_table::PTEFlags::W) => {}
                _ => {
                    put_user_pages(&ppns);
                    return Err(i);
                }
            }
        }

        // 查找页表映射 → 物理页号
        let ppn = match pt.translate(vpn) {
            Some(ppn) => ppn,
            None => {
                put_user_pages(&ppns);
                return Err(i);
            }
        };

        // 增加 MetaSlot 引用计数
        if let Some(slot) = page_meta::try_meta_of(ppn) {
            slot.inc_ref();

            // Pin 模式：设置 PG_LOCKED
            if flags.contains(&GupFlags::FOLL_PIN) {
                slot.set_flag(frame_flags::PG_LOCKED);
            }
        }

        ppns.push(ppn);
    }

    Ok(ppns)
}

/// 释放 `get_user_pages` 获取的用户页引用。
///
/// 对每个 PPN 的 MetaSlot ref_count -1。若 ref_count 降到 0，帧自动归还 buddy。
pub fn put_user_pages(ppns: &[PhysPageNum]) {
    for &ppn in ppns {
        if let Some(slot) = page_meta::try_meta_of(ppn) {
            if slot.dec_ref() {
                slot.finalize_release();
                super::frame::return_frame_to_buddy(ppn, 0);
            }
        }
    }
}

/// Pin 用户页（获取引用 + 设置 PG_LOCKED）。
///
/// 等价于 `get_user_pages(..., FOLL_PIN)`。
pub fn pin_user_pages(
    address_space: &AddressSpace,
    start_vpn: super::VirtPageNum,
    nr_pages: usize,
) -> Result<alloc::vec::Vec<PhysPageNum>, usize> {
    get_user_pages(
        address_space,
        start_vpn,
        nr_pages,
        GupFlags::FOLL_PIN,
    )
}

/// Unpin 用户页（释放引用 + 清除 PG_LOCKED）。
pub fn unpin_user_pages(ppns: &[PhysPageNum]) {
    for &ppn in ppns {
        if let Some(slot) = page_meta::try_meta_of(ppn) {
            slot.clear_flag(frame_flags::PG_LOCKED);
            if slot.dec_ref() {
                slot.finalize_release();
                super::frame::return_frame_to_buddy(ppn, 0);
            }
        }
    }
}
