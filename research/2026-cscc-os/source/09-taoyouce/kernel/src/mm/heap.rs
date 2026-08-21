//! 内核堆分配器
//! 
//! 使用链表分配器实现真正的内存回收

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use core::sync::atomic::Ordering;
use crate::sync::SpinLock;

/// 内核堆大小（96MB）- 支持 libc-bench 大缓冲区测试和大型用户程序
/// 历史教训：32MB 在 libc-bench b_stdio_putcgetc 触发 RamFs::write 4MB Vec::resize 时
/// 因为链表 first-fit 分配器碎片化导致 OOM。扩到 96MB 给足头部。
/// 注意：堆在 BSS 段中，过大会导致 QEMU ROM 区域重叠错误。
/// 96MB BSS 在 RV (256M memory) 和 LA (1G memory) 都安全。
const KERNEL_HEAP_SIZE: usize = 96 * 1024 * 1024;

/// 内核堆内存
#[repr(align(4096))]
struct HeapSpace([u8; KERNEL_HEAP_SIZE]);

// 放进链接脚本的 .bss.noinit（NOLOAD、位于 sbss..ebss 之外）：96MB 堆空间
// 不参与 boot BSS 清零（省 ~39M 条清零指令）。链表堆 init 自建表头、
// alloc_zeroed 由 GlobalAlloc 默认实现补零，均不依赖初始为零。
#[link_section = ".bss.noinit"]
static mut HEAP_SPACE: HeapSpace = HeapSpace([0; KERNEL_HEAP_SIZE]);

/// 空闲块头部：内嵌 AVL 节点（按地址排序）。
///
/// 旧实现是单向有序链表：`dealloc` 为了找到插入点必须从头线性扫。BuildStorm
/// LA toolchain 实测空闲链表碎裂到 12108 个节点，`cargo --version` 之后一次
/// 高位地址释放就在 `insert_free_block` 里空转数分钟（GDB 现场：heap.rs:118）。
/// 这里把链表升级为父指针 + 高度平衡的内嵌 AVL，插入/删除/合并 O(log n)，
/// `find_fit` 仍按地址序做 first-fit（用 parent 指针求中序后继，零额外分配）。
#[repr(C)]
struct FreeBlock {
    size: usize,
    left: Option<NonNull<FreeBlock>>,
    right: Option<NonNull<FreeBlock>>,
    parent: Option<NonNull<FreeBlock>>,
    height: u8,
}

impl FreeBlock {
    /// 最小块大小（必须能容纳 FreeBlock 结构）
    const MIN_SIZE: usize = core::mem::size_of::<FreeBlock>();
}

/// 链表堆分配器
/// 
/// 使用首次适配（first-fit）策略的空闲链表分配器
struct LinkedListAllocator {
    root: Option<NonNull<FreeBlock>>,
    heap_start: usize,
    heap_end: usize,
    total_allocated: usize,
    allocation_count: usize,
    /// 运行期通过向帧池借页扩堆累计获得的字节数（Fix C）。
    grown_bytes: usize,
    /// 诊断：最近一次 alloc/dealloc 的调用者与大小。
    last_alloc_ra: usize,
    last_alloc_size: usize,
    last_dealloc_ra: usize,
    last_dealloc_size: usize,
    last_dealloc_ptr: usize,
    last_dealloc_outer_ra: usize,
}

unsafe impl Send for LinkedListAllocator {}

impl LinkedListAllocator {
    const fn new() -> Self {
        Self {
            root: None,
            heap_start: 0,
            heap_end: 0,
            total_allocated: 0,
            allocation_count: 0,
            grown_bytes: 0,
            last_alloc_ra: 0,
            last_alloc_size: 0,
            last_dealloc_ra: 0,
            last_dealloc_size: 0,
            last_dealloc_ptr: 0,
            last_dealloc_outer_ra: 0,
        }
    }
    
    /// 初始化分配器
    fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.heap_start = heap_start;
        self.heap_end = heap_start + heap_size;
        self.root = None;
        self.insert_free_block(heap_start, heap_size);
    }

    fn insert_free_block(&mut self, block_start: usize, size: usize) {
        if !valid_free_block_addr(block_start) || size == 0 || size > (1usize << 30) {
            crate::console::raw_emergency_print(format_args!(
                "[HEAPDBG] insert invalid block start={:#x} size={:#x} heap=[{:#x},{:#x}) grown={} last_alloc_ra={:#x} last_alloc_size={:#x} last_dealloc_ra={:#x} last_dealloc_size={:#x} last_dealloc_ptr={:#x}
",
                block_start, size, self.heap_start, self.heap_end, self.grown_bytes,
                self.last_alloc_ra, self.last_alloc_size,
                self.last_dealloc_ra, self.last_dealloc_size, self.last_dealloc_ptr,
            ));
            panic!("old-heap free block corrupted before insert");
        }
        if size < FreeBlock::MIN_SIZE {
            return;
        }

        // 有序 AVL：先与地址序前驱/后继物理合并，再 O(log n) 插入。
        unsafe {
            let mut start = block_start;
            let mut len = size;

            if let Some(pred) = Self::predecessor_addr(self.root, block_start) {
                let pred_end = Self::addr_of(pred) + (*pred.as_ptr()).size;
                if pred_end > block_start {
                    crate::console::raw_emergency_print(format_args!(
                        "[HEAPDBG] overlap with predecessor pred={:#x} size={:#x} new={:#x} size={:#x}\n",
                        Self::addr_of(pred), (*pred.as_ptr()).size, block_start, size
                    ));
                    panic!("old-heap free block overlap (predecessor)");
                }
                if pred_end == block_start {
                    start = Self::addr_of(pred);
                    len += (*pred.as_ptr()).size;
                    let (new_root, removed) = Self::avl_remove(self.root, Self::addr_of(pred));
                    self.root = new_root;
                    debug_assert!(removed.is_some());
                }
            }

            if let Some(succ) = Self::successor_addr(self.root, block_start) {
                if start + len > Self::addr_of(succ) {
                    crate::console::raw_emergency_print(format_args!(
                        "[HEAPDBG] overlap with successor succ={:#x} size={:#x} merged={:#x} size={:#x} dealloc_ra={:#x} dealloc_size={:#x} dealloc_ptr={:#x} outer_ra={:#x} insert_ra={:#x}\n",
                        Self::addr_of(succ), (*succ.as_ptr()).size, start, len,
                        self.last_dealloc_ra, self.last_dealloc_size, self.last_dealloc_ptr,
                        self.last_dealloc_outer_ra, caller_ra()
                    ));
                    panic!("old-heap free block overlap (successor)");
                }
                if start + len == Self::addr_of(succ) {
                    len += (*succ.as_ptr()).size;
                    let (new_root, removed) = Self::avl_remove(self.root, Self::addr_of(succ));
                    self.root = new_root;
                    debug_assert!(removed.is_some());
                }
            }

            if Self::find_node(self.root, start).is_some() {
                crate::console::raw_emergency_print(format_args!(
                    "[HEAPDBG] double free addr={:#x} size={:#x}\n", start, len));
                panic!("old-heap double free");
            }

            let node = NonNull::new_unchecked(start as *mut FreeBlock);
            (*node.as_ptr()).size = len;
            self.root = Some(Self::avl_insert(self.root, node));
        }
    }

    // ================= AVL helpers =================

    #[inline]
    fn addr_of(node: NonNull<FreeBlock>) -> usize {
        node.as_ptr() as usize
    }

    #[inline]
    unsafe fn height_of(node: Option<NonNull<FreeBlock>>) -> u8 {
        node.map_or(0, |n| (*n.as_ptr()).height)
    }

    #[inline]
    unsafe fn refresh(node: NonNull<FreeBlock>) {
        let lh = Self::height_of((*node.as_ptr()).left);
        let rh = Self::height_of((*node.as_ptr()).right);
        (*node.as_ptr()).height = 1 + lh.max(rh);
    }

    #[inline]
    unsafe fn balance_factor(node: NonNull<FreeBlock>) -> i8 {
        Self::height_of((*node.as_ptr()).left) as i8
            - Self::height_of((*node.as_ptr()).right) as i8
    }

    unsafe fn rotate_left(mut x: NonNull<FreeBlock>) -> NonNull<FreeBlock> {
        let y = (*x.as_ptr()).right.expect("heap AVL rotate-left without right child");
        let beta = (*y.as_ptr()).left;
        // 关键：新根 y 必须继承旧根 x 的 parent。若漏掉这一步，
        // 树根旋转后 root.parent 仍指回旧根，形成 parent 环；
        // find_fit 的中序后继遍历会 x→y→x 死循环（LA net-stack
        // 初始化就是卡在 DNS socket 之后的下一次 successor 遍历）。
        let parent = (*x.as_ptr()).parent;
        (*x.as_ptr()).right = beta;
        if let Some(b) = beta {
            (*b.as_ptr()).parent = Some(x);
        }
        (*y.as_ptr()).left = Some(x);
        (*x.as_ptr()).parent = Some(y);
        (*y.as_ptr()).parent = parent;
        Self::refresh(x);
        Self::refresh(y);
        y
    }

    unsafe fn rotate_right(mut x: NonNull<FreeBlock>) -> NonNull<FreeBlock> {
        let y = (*x.as_ptr()).left.expect("heap AVL rotate-right without left child");
        let beta = (*y.as_ptr()).right;
        // 同 rotate_left：新根继承旧根的 parent，杜绝旋转后
        // 树根 parent 指针成环导致的空闲链表中序遍历死循环。
        let parent = (*x.as_ptr()).parent;
        (*x.as_ptr()).left = beta;
        if let Some(b) = beta {
            (*b.as_ptr()).parent = Some(x);
        }
        (*y.as_ptr()).right = Some(x);
        (*x.as_ptr()).parent = Some(y);
        (*y.as_ptr()).parent = parent;
        Self::refresh(x);
        Self::refresh(y);
        y
    }

    unsafe fn balance(mut node: NonNull<FreeBlock>) -> NonNull<FreeBlock> {
        Self::refresh(node);
        let bf = Self::balance_factor(node);
        if bf > 1 {
            let l = (*node.as_ptr()).left.expect("heap AVL left child");
            if Self::balance_factor(l) < 0 {
                let new_l = Self::rotate_left(l);
                (*node.as_ptr()).left = Some(new_l);
                (*new_l.as_ptr()).parent = Some(node);
            }
            return Self::rotate_right(node);
        }
        if bf < -1 {
            let r = (*node.as_ptr()).right.expect("heap AVL right child");
            if Self::balance_factor(r) > 0 {
                let new_r = Self::rotate_right(r);
                (*node.as_ptr()).right = Some(new_r);
                (*new_r.as_ptr()).parent = Some(node);
            }
            return Self::rotate_left(node);
        }
        node
    }

    unsafe fn avl_insert(
        root: Option<NonNull<FreeBlock>>,
        node: NonNull<FreeBlock>,
    ) -> NonNull<FreeBlock> {
        (*node.as_ptr()).left = None;
        (*node.as_ptr()).right = None;
        (*node.as_ptr()).parent = None;
        (*node.as_ptr()).height = 1;
        let Some(mut cur) = root else {
            return node;
        };
        let key = Self::addr_of(node);
        let cur_key = Self::addr_of(cur);
        if key < cur_key {
            let child = Self::avl_insert((*cur.as_ptr()).left, node);
            (*cur.as_ptr()).left = Some(child);
            (*child.as_ptr()).parent = Some(cur);
        } else if key > cur_key {
            let child = Self::avl_insert((*cur.as_ptr()).right, node);
            (*cur.as_ptr()).right = Some(child);
            (*child.as_ptr()).parent = Some(cur);
        } else {
            crate::console::raw_emergency_print(format_args!(
                "[HEAPDBG] duplicate free block addr={:#x}\n", key));
            panic!("old-heap duplicate free block");
        }
        Self::balance(cur)
    }

    unsafe fn avl_remove_min(
        root: Option<NonNull<FreeBlock>>,
    ) -> (Option<NonNull<FreeBlock>>, NonNull<FreeBlock>) {
        let Some(mut cur) = root else {
            unreachable!("heap AVL remove-min on empty tree");
        };
        if (*cur.as_ptr()).left.is_none() {
            let right = (*cur.as_ptr()).right;
            if let Some(r) = right {
                (*r.as_ptr()).parent = (*cur.as_ptr()).parent;
            }
            return (right, cur);
        }
        let (new_left, min) = Self::avl_remove_min((*cur.as_ptr()).left);
        (*cur.as_ptr()).left = new_left;
        if let Some(l) = new_left {
            (*l.as_ptr()).parent = Some(cur);
        }
        (Some(Self::balance(cur)), min)
    }

    unsafe fn avl_remove(
        root: Option<NonNull<FreeBlock>>,
        key: usize,
    ) -> (Option<NonNull<FreeBlock>>, Option<NonNull<FreeBlock>>) {
        let Some(mut cur) = root else {
            return (None, None);
        };
        let cur_key = Self::addr_of(cur);
        if key < cur_key {
            let (new_left, removed) = Self::avl_remove((*cur.as_ptr()).left, key);
            (*cur.as_ptr()).left = new_left;
            if let Some(l) = new_left {
                (*l.as_ptr()).parent = Some(cur);
            }
            return (Some(Self::balance(cur)), removed);
        }
        if key > cur_key {
            let (new_right, removed) = Self::avl_remove((*cur.as_ptr()).right, key);
            (*cur.as_ptr()).right = new_right;
            if let Some(r) = new_right {
                (*r.as_ptr()).parent = Some(cur);
            }
            return (Some(Self::balance(cur)), removed);
        }
        let removed = Some(cur);
        let parent = (*cur.as_ptr()).parent;
        match ((*cur.as_ptr()).left, (*cur.as_ptr()).right) {
            (None, right) => {
                if let Some(r) = right {
                    (*r.as_ptr()).parent = parent;
                }
                (right, removed)
            }
            (left, None) => {
                if let Some(l) = left {
                    (*l.as_ptr()).parent = parent;
                }
                (left, removed)
            }
            (Some(left), Some(right)) => {
                let (new_right, mut succ) = Self::avl_remove_min(Some(right));
                (*succ.as_ptr()).left = Some(left);
                (*left.as_ptr()).parent = Some(succ);
                (*succ.as_ptr()).right = new_right;
                if let Some(r) = new_right {
                    (*r.as_ptr()).parent = Some(succ);
                }
                (*succ.as_ptr()).parent = parent;
                (Some(Self::balance(succ)), removed)
            }
        }
    }

    unsafe fn leftmost(mut node: NonNull<FreeBlock>) -> NonNull<FreeBlock> {
        while let Some(l) = (*node.as_ptr()).left {
            node = l;
        }
        node
    }

    unsafe fn rightmost(mut node: NonNull<FreeBlock>) -> NonNull<FreeBlock> {
        while let Some(r) = (*node.as_ptr()).right {
            node = r;
        }
        node
    }

    unsafe fn successor(node: NonNull<FreeBlock>) -> Option<NonNull<FreeBlock>> {
        if let Some(r) = (*node.as_ptr()).right {
            return Some(Self::leftmost(r));
        }
        let mut cur = node;
        loop {
            let parent = (*cur.as_ptr()).parent?;
            let parent_right_is_cur = (*parent.as_ptr())
                .right
                .map_or(false, |r| r.as_ptr() == cur.as_ptr());
            if parent_right_is_cur {
                cur = parent;
            } else {
                return Some(parent);
            }
        }
    }

    unsafe fn find_node(
        mut root: Option<NonNull<FreeBlock>>,
        key: usize,
    ) -> Option<NonNull<FreeBlock>> {
        while let Some(cur) = root {
            let cur_key = Self::addr_of(cur);
            if key == cur_key {
                return Some(cur);
            }
            root = if key < cur_key {
                (*cur.as_ptr()).left
            } else {
                (*cur.as_ptr()).right
            };
        }
        None
    }

    unsafe fn predecessor_addr(
        mut root: Option<NonNull<FreeBlock>>,
        addr: usize,
    ) -> Option<NonNull<FreeBlock>> {
        let mut best = None;
        while let Some(cur) = root {
            let cur_key = Self::addr_of(cur);
            if cur_key < addr {
                best = Some(cur);
                root = (*cur.as_ptr()).right;
            } else {
                root = (*cur.as_ptr()).left;
            }
        }
        best
    }

    unsafe fn successor_addr(
        mut root: Option<NonNull<FreeBlock>>,
        addr: usize,
    ) -> Option<NonNull<FreeBlock>> {
        let mut best = None;
        while let Some(cur) = root {
            let cur_key = Self::addr_of(cur);
            if cur_key > addr {
                best = Some(cur);
                root = (*cur.as_ptr()).left;
            } else {
                root = (*cur.as_ptr()).right;
            }
        }
        best
    }

    /// 首次适配（first-fit）在空闲 AVL 中按地址序查找可用块；不触发扩堆。
    fn find_fit(&mut self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(FreeBlock::MIN_SIZE);
        let align = layout.align().max(core::mem::align_of::<FreeBlock>());

        let mut current = self.root.map(|r| unsafe { Self::leftmost(r) });
        while let Some(block_ptr) = current {
            let block_start = block_ptr.as_ptr() as usize;
            if !valid_free_block_addr(block_start) {
                crate::console::raw_emergency_print(format_args!(
                    "[HEAPDBG] corrupt freelist node={:#x} heap=[{:#x},{:#x}) grown={} last_alloc_ra={:#x} last_alloc_size={:#x} last_dealloc_ra={:#x} last_dealloc_size={:#x} last_dealloc_ptr={:#x}\n",
                    block_start, self.heap_start, self.heap_end, self.grown_bytes,
                    self.last_alloc_ra, self.last_alloc_size,
                    self.last_dealloc_ra, self.last_dealloc_size, self.last_dealloc_ptr,
                ));
                panic!("old-heap freelist corrupted");
            }
            let block_size = unsafe { (*block_ptr.as_ptr()).size };
            if block_size == 0 || block_size > (1usize << 30) || block_start + block_size > block_start + (1usize << 31) {
                crate::console::raw_emergency_print(format_args!(
                    "[HEAPDBG] corrupt block size start={:#x} size={:#x} last_dealloc_ra={:#x} last_dealloc_size={:#x}\n",
                    block_start, block_size, self.last_dealloc_ra, self.last_dealloc_size,
                ));
                panic!("old-heap block size corrupted");
            }

            let alloc_start = align_up(block_start, align);
            let alloc_end = alloc_start.saturating_add(size);
            if alloc_end <= block_start + block_size {
                let excess_start = alloc_end;
                let excess_size = (block_start + block_size) - alloc_end;
                unsafe {
                    let (new_root, removed) = Self::avl_remove(self.root, block_start);
                    self.root = new_root;
                    debug_assert!(removed.is_some());
                }
                if excess_size >= FreeBlock::MIN_SIZE {
                    self.insert_free_block(excess_start, excess_size);
                }
                let padding_start = block_start;
                let padding_size = alloc_start - block_start;
                if padding_size >= FreeBlock::MIN_SIZE {
                    self.insert_free_block(padding_start, padding_size);
                }
                self.total_allocated += size;
                self.allocation_count += 1;
                return alloc_start as *mut u8;
            }

            current = unsafe { Self::successor(block_ptr) };
        }
        core::ptr::null_mut()
    }

    /// 分配内存：先在现有空闲链表首次适配；失败则按需向帧池借页扩堆后重试。
    /// 对标 Linux「kmalloc 经 slab/buddy 从同一 page allocator 取页」，打通
    /// 「96MB 固定堆孤岛」与「~922MB 帧池」两套原本隔离的内存池。
    ///
    /// 锁序严格 HEAP→FRAME（本方法已持堆锁，再取帧锁），buddy 空闲链表已预留满
    /// 容量故借帧全程不回调堆，不存在 HEAP→FRAME→HEAP 自死锁。
    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        self.last_alloc_ra = caller_ra();
        self.last_alloc_size = layout.size();
        let ptr = self.find_fit(layout);
        if !ptr.is_null() {
            return ptr;
        }
        // 现有堆空间不足：向帧池借连续页扩堆后重试；try_grow 内部会逐阶缩小，
        // 这里的多次尝试覆盖「借到的块仍因对齐/碎片不满足」的极少数情况。
        let mut attempts = 0;
        while attempts < 4 {
            if !self.try_grow(layout) {
                break;
            }
            let ptr = self.find_fit(layout);
            if !ptr.is_null() {
                return ptr;
            }
            attempts += 1;
        }
        core::ptr::null_mut()
    }

    /// 向帧分配器借一段连续物理页，永久并入内核堆空闲链表；成功返回 true。
    /// 单次至少借 `MIN_GROW_PAGES`(2MiB) 摊薄借页频率；请求更大时借到能容纳
    /// 「请求 + 对齐冗余 + 块头」的最小 2 的幂。该阶借不到时逐阶缩小重试，缩到
    /// 低于实际需求即放弃（交由调用方做优雅 OOM，绝不 panic）。
    fn try_grow(&mut self, layout: Layout) -> bool {
        let page = crate::arch::consts::PAGE_SIZE;
        let need_bytes = layout
            .size()
            .max(FreeBlock::MIN_SIZE)
            .saturating_add(layout.align())
            .saturating_add(FreeBlock::MIN_SIZE);
        let need_pages = (need_bytes + page - 1) / page;
        let min_pages = need_pages.next_power_of_two();
        const MIN_GROW_PAGES: usize = 512; // 2MiB / 4KiB
        let mut grow_pages = need_pages.max(MIN_GROW_PAGES).next_power_of_two();
        loop {
            let order = grow_pages.trailing_zeros() as usize;
            if let Some((vaddr, len)) = super::frame::alloc_kernel_heap_region(order) {
                self.grown_bytes += len;
                self.insert_free_block(vaddr, len);
                return true;
            }
            if grow_pages <= min_pages {
                return false;
            }
            grow_pages >>= 1;
        }
    }

    /// 释放内存
    fn dealloc(&mut self, ptr: *mut u8, layout: Layout, outer_ra: usize) {
        let size = layout.size().max(FreeBlock::MIN_SIZE);
        let block_start = ptr as usize;
        self.last_dealloc_ra = caller_ra();
        self.last_dealloc_outer_ra = outer_ra;
        self.last_dealloc_size = layout.size();
        self.last_dealloc_ptr = block_start;

        self.insert_free_block(block_start, size);
        self.total_allocated = self.total_allocated.saturating_sub(size);
        self.allocation_count = self.allocation_count.saturating_sub(1);
    }
}

/// 对齐到指定边界
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// 调用者返回地址（诊断用）。
#[inline(always)]
fn caller_ra() -> usize {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        let ra: usize;
        core::arch::asm!("mv {}, ra", out(reg) ra, options(nomem, nostack));
        ra
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        let ra: usize;
        core::arch::asm!("move {}, $ra", out(reg) ra, options(nomem, nostack));
        ra
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    { 0 }
}

/// 内核直映射地址的最小合法值：任何空闲块/分配指针低于它都必然是元数据被写坏。
const MIN_KERNEL_ADDR: usize = 0x8000_0000;

#[inline]
fn valid_free_block_addr(addr: usize) -> bool {
    addr >= MIN_KERNEL_ADDR && addr % core::mem::align_of::<FreeBlock>() == 0
}

/// 全局堆分配器
struct LockedHeap(SpinLock<LinkedListAllocator>);

impl LockedHeap {
    const fn new() -> Self {
        Self(SpinLock::new(LinkedListAllocator::new()))
    }
}

/// 大对象（> SLUB 上限 2048B）直通 buddy 页分配的阶数：块大小取
/// max(size, align) 向上取整页数的 2 的幂（buddy 块天然按自身尺寸对齐）。
fn large_order(size: usize, align: usize) -> usize {
    let page = crate::arch::consts::PAGE_SIZE;
    let bytes = size.max(align);
    let pages = (bytes + page - 1) / page;
    pages.next_power_of_two().trailing_zeros() as usize
}

/// 内核直映射虚拟地址 → 物理地址（RV 恒等；LA 去 DMW0 前缀）。
#[inline]
fn kernel_vaddr_to_paddr(vaddr: usize) -> usize {
    #[cfg(target_arch = "riscv64")]
    { vaddr }
    #[cfg(target_arch = "loongarch64")]
    { vaddr & 0x0000_FFFF_FFFF_FFFF }
}

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        // SLUB 快速路径：≤2048B 且 SLUB 已初始化 → O(1) 分配
        if size <= super::slab::MAX_SLAB_OBJ_SIZE
            && super::slab::SLUB_INITIALIZED.load(Ordering::Acquire)
            && align <= super::slab::slab_class_size(size).unwrap_or(0)
        {
            if let Some(ptr) = super::slab::slub_alloc(size, align) {
                return ptr;
            }
        }

        // 大对象直通 buddy 页分配（对标 Linux kmalloc_large → alloc_pages）：
        // 链表堆的 first-fit 扫描 + 有序插入都是 O(空闲块数)，网络/文件路径的
        // 大 Vec 洪泛把空闲链表打碎后，每次大分配/释放都要线性扫上千块（iperf
        // UDP 灌洪后 TCP 掉到 1/30 吞吐的根因）。buddy 弹链表头是 O(1)，且
        // 2 的幂块自动合并、不留长碎片链。SLUB_INITIALIZED 隐含帧池已就绪。
        if size > super::slab::MAX_SLAB_OBJ_SIZE
            && super::slab::SLUB_INITIALIZED.load(Ordering::Acquire)
        {
            let order = large_order(size, align);
            if let Some(ppn) = super::frame::alloc_frames_uninit(order) {
                return ppn.kernel_addr() as *mut u8;
            }
            // 帧池碎片/耗尽：退回链表堆兜底
        }

        // 小对象 SLUB 未初始化 / 分配失败：走老链表堆
        self.0.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        let outer_ra = caller_ra();

        // 1. 检查是否在 96MB 静态堆空间内 → 老堆 dealloc
        let addr = ptr as usize;
        let heap_start = HEAP_SPACE.0.as_ptr() as usize;
        let heap_end = heap_start + KERNEL_HEAP_SIZE;
        if addr >= heap_start && addr < heap_end {
            self.0.lock().dealloc(ptr, layout, outer_ra);
            return;
        }

        // 2. SLUB 分配的对象：优先读 MetaSlot allocator cookie（O(1)，
        //    Linux page->slab_cache 的轻量替代），魔数/owner 仅作防御校验。
        if super::slab::SLUB_INITIALIZED.load(Ordering::Acquire) {
            let ppn = super::PhysAddr(kernel_vaddr_to_paddr(addr)).floor();
            let cookie = super::page_meta::meta_of(ppn).allocator();
            if cookie == super::page_meta::AllocCookie::Slab
                && super::slab::is_slab_page(ptr)
            {
                super::slab::slub_dealloc(ptr, size);
                return;
            }
        }

        // 3. 大对象：cookie == Buddy → 本文件 alloc 的 buddy 直通块，
        //    按同一 order 公式归还 buddy。cookie Unknown/Heap 是静态堆或
        //    try_grow 借页并入链表堆的增长区，仍走老堆 dealloc。
        if size > super::slab::MAX_SLAB_OBJ_SIZE
            && super::slab::SLUB_INITIALIZED.load(Ordering::Acquire)
        {
            let ppn = super::PhysAddr(kernel_vaddr_to_paddr(addr)).floor();
            let slot = super::page_meta::meta_of(ppn);
            if slot.allocator() == super::page_meta::AllocCookie::Buddy
                && slot.get_owner() == super::page_meta::FrameOwner::User
            {
                super::frame::dealloc_frames(ppn, large_order(size, layout.align()));
                return;
            }
        }

        // 4. 其余（try_grow 借来的页等）→ 老堆 dealloc
        self.0.lock().dealloc(ptr, layout, outer_ra);
    }
}

#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap = LockedHeap::new();

/// 初始化内核堆
pub fn init_heap() {
    unsafe {
        let heap_start = HEAP_SPACE.0.as_ptr() as usize;
        HEAP_ALLOCATOR.0.lock().init(heap_start, KERNEL_HEAP_SIZE);
    }
    
    #[cfg(feature = "debug")]
    crate::println!("[heap] 初始化完成: {} MB", KERNEL_HEAP_SIZE / 1024 / 1024);
}

/// 获取堆使用统计
pub fn heap_stats() -> (usize, usize, usize, usize, usize) {
    let allocator = HEAP_ALLOCATOR.0.lock();
    let mut free_total = 0usize;
    let mut largest_free = 0usize;
    let mut current = allocator.root.map(|r| unsafe { LinkedListAllocator::leftmost(r) });
    while let Some(block_ptr) = current {
        let block = unsafe { block_ptr.as_ref() };
        free_total += block.size;
        if block.size > largest_free {
            largest_free = block.size;
        }
        current = unsafe { LinkedListAllocator::successor(block_ptr) };
    }
    (
        allocator.total_allocated,
        KERNEL_HEAP_SIZE + allocator.grown_bytes,
        allocator.allocation_count,
        free_total,
        largest_free,
    )
}

pub fn heap_size_bins() -> [usize; 16] {
    [0; 16]
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::Layout;
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    
    #[test_case]
    fn test_heap_alloc() {
        let x = Box::new(42);
        assert_eq!(*x, 42);
    }
    
    #[test_case]
    fn test_vec_alloc() {
        let mut v = Vec::new();
        for i in 0..100 {
            v.push(i);
        }
        assert_eq!(v.len(), 100);
    }
    
    #[test_case]
    fn test_alloc_dealloc_cycle() {
        for _ in 0..1000 {
            let v: Vec<u8> = (0..100).collect();
            drop(v);
        }
    }

    #[test_case]
    fn test_allocator_coalesces_adjacent_blocks() {
        #[repr(align(4096))]
        struct TestHeap([u8; 16 * 1024]);

        let mut heap_space = TestHeap([0; 16 * 1024]);
        let mut allocator = LinkedListAllocator::new();
        allocator.init(heap_space.0.as_mut_ptr() as usize, heap_space.0.len());

        let page = Layout::from_size_align(4096, 1).unwrap();
        let first = allocator.alloc(page);
        let second = allocator.alloc(page);

        assert!(!first.is_null());
        assert!(!second.is_null());

        allocator.dealloc(first, page, 0);
        allocator.dealloc(second, page, 0);

        let two_pages = Layout::from_size_align(8192, 1).unwrap();
        let merged = allocator.alloc(two_pages);
        assert!(!merged.is_null());
    }
}
