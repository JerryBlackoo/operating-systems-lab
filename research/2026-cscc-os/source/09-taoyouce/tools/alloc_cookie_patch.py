from pathlib import Path
p=Path('/workspace/kernel/src/mm/page_meta.rs')
s=p.read_text()
old="""    /// 所属 compound 块的 order（内核栈 order-4 = 16 页等）。
    /// 仅在 compound head 帧有意义，tail 帧的 order 为 0。
    order: AtomicU8,

    /// 保留对齐（使 MetaSlot 严格 8 字节）
    _pad: u8,
"""
new="""    /// 所属 compound 块的 order（内核栈 order-4 = 16 页等）。
    /// 仅在 compound head 帧有意义，tail 帧的 order 为 0。
    order: AtomicU8,

    /// Allocator cookie（Linux `page->slab_cache` 的轻量替代）：
    /// 0=Unknown，1=Buddy 大对象直通，2=SLUB slab 页。GlobalAlloc 的
    /// dealloc 用它做 O(1) 回收路由，不再依赖地址区间/魔数猜路径。
    allocator: AtomicU8,
"""
assert old in s
s=s.replace(old,new,1)
old="""            order: AtomicU8::new(0),
            _pad: 0,
            map_count: AtomicU16::new(0),
"""
new="""            order: AtomicU8::new(0),
            allocator: AtomicU8::new(AllocCookie::Unknown as u8),
            map_count: AtomicU16::new(0),
"""
assert old in s
s=s.replace(old,new,1)
# Add enum before struct
old="""// ── MetaSlot 结构体 ─────────────────────────────────────────────────────
"""
new="""// ── Allocator cookie ─────────────────────────────────────────────────────

/// 帧当前属于哪个内核分配器路径。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocCookie {
    /// 静态堆 / 未分类 / buddy 空闲
    Unknown = 0,
    /// `kmalloc_large`：由 GlobalAlloc 直通 buddy 的大对象
    Buddy = 1,
    /// SLUB slab 页
    Slab = 2,
}

impl AllocCookie {
    #[inline]
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Buddy,
            2 => Self::Slab,
            _ => Self::Unknown,
        }
    }
}

// ── MetaSlot 结构体 ─────────────────────────────────────────────────────
"""
assert old in s
s=s.replace(old,new,1)
# Reset cookie on releases
s=s.replace("""            self.owner.store(FrameOwner::Free as u8, Ordering::Release);
            self.flags.store(0, Ordering::Release);
            self.order.store(0, Ordering::Relaxed);
            true""","""            self.owner.store(FrameOwner::Free as u8, Ordering::Release);
            self.flags.store(0, Ordering::Release);
            self.order.store(0, Ordering::Relaxed);
            self.allocator.store(AllocCookie::Unknown as u8, Ordering::Relaxed);
            true""")
s=s.replace("""        self.owner.store(FrameOwner::Free as u8, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.order.store(0, Ordering::Relaxed);
    }
""","""        self.owner.store(FrameOwner::Free as u8, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.order.store(0, Ordering::Relaxed);
        self.allocator.store(AllocCookie::Unknown as u8, Ordering::Relaxed);
    }
""")
s=s.replace("""        self.owner.store(FrameOwner::Free as u8, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.order.store(0, Ordering::Relaxed);
    }
""","""        self.owner.store(FrameOwner::Free as u8, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.order.store(0, Ordering::Relaxed);
        self.allocator.store(AllocCookie::Unknown as u8, Ordering::Relaxed);
    }
""")
# Add methods near owner ops
old="""    /// 设置帧用途（仅在独占或首次初始化时调用）。
    #[inline]
    pub fn set_owner(&self, owner: FrameOwner) {
        self.owner.store(owner as u8, Ordering::Release);
    }
"""
new="""    /// 设置帧用途（仅在独占或首次初始化时调用）。
    #[inline]
    pub fn set_owner(&self, owner: FrameOwner) {
        self.owner.store(owner as u8, Ordering::Release);
    }

    /// 设置 allocator cookie（仅在 owner 初始化/切换时调用）。
    #[inline]
    pub fn set_allocator(&self, cookie: AllocCookie) {
        self.allocator.store(cookie as u8, Ordering::Release);
    }

    /// 读取 allocator cookie。
    #[inline]
    pub fn allocator(&self) -> AllocCookie {
        AllocCookie::from_u8(self.allocator.load(Ordering::Acquire))
    }
"""
assert old in s
s=s.replace(old,new,1)
p.write_text(s)

# frame: set Buddy cookie on direct large/frame allocations
p=Path('/workspace/kernel/src/mm/frame.rs')
s=p.read_text()
old="""        page_meta::batch_init_shared(ppn, page_count, FrameOwner::User, order as u8);

        // 清零
        zero_frame(ppn, page_count);
"""
new="""        page_meta::batch_init_shared(ppn, page_count, FrameOwner::User, order as u8);
        page_meta::meta_of(ppn).set_allocator(page_meta::AllocCookie::Buddy);

        // 清零
        zero_frame(ppn, page_count);
"""
assert old in s
s=s.replace(old,new,1)
old="""        page_meta::batch_init_shared(ppn, page_count, FrameOwner::User, order as u8);
        ppn
"""
new="""        page_meta::batch_init_shared(ppn, page_count, FrameOwner::User, order as u8);
        page_meta::meta_of(ppn).set_allocator(page_meta::AllocCookie::Buddy);
        ppn
"""
assert old in s
s=s.replace(old,new,1)
# heap growth cookie
old="""    page_meta::batch_init_heap(ppn, page_count);
"""
new="""    page_meta::batch_init_heap(ppn, page_count);
    page_meta::meta_of(ppn).set_allocator(page_meta::AllocCookie::Unknown);
"""
assert old in s
s=s.replace(old,new,1)
p.write_text(s)

# slab cookie
p=Path('/workspace/kernel/src/mm/slab.rs')
s=p.read_text()
old="""    let ppn = super::frame::alloc_frame()?;
    // MetaSlot 标记为 Slab 用途（alloc_frame 默认设 User，此处精确化）
    crate::mm::page_meta::meta_of(ppn).set_owner(crate::mm::page_meta::FrameOwner::Slab);
    Some(ppn.kernel_addr())
"""
new="""    let ppn = super::frame::alloc_frame()?;
    // MetaSlot 标记为 Slab 用途（alloc_frame 默认设 User，此处精确化），
    // 并写入 allocator cookie 供 GlobalAlloc::dealloc O(1) 路由。
    let slot = crate::mm::page_meta::meta_of(ppn);
    slot.set_owner(crate::mm::page_meta::FrameOwner::Slab);
    slot.set_allocator(crate::mm::page_meta::AllocCookie::Slab);
    Some(ppn.kernel_addr())
"""
assert old in s
s=s.replace(old,new,1)
p.write_text(s)

# heap GlobalAlloc dealloc cookie routing
p=Path('/workspace/kernel/src/mm/heap.rs')
s=p.read_text()
old="""        // 2. SLUB 分配的对象（slab 页有 SLAB_MAGIC 魔数）
        if size <= super::slab::MAX_SLAB_OBJ_SIZE
            && super::slab::is_slab_page(ptr)
        {
            super::slab::slub_dealloc(ptr, size);
            return;
        }

        // 3. 大对象：静态堆外且 MetaSlot owner=User → 本文件 alloc 的
        //    buddy 直通块，按同一 order 公式归还 buddy。owner=Heap 的是
        //    try_grow 借页并入链表堆的增长区，仍走老堆 dealloc。
        if size > super::slab::MAX_SLAB_OBJ_SIZE
            && super::slab::SLUB_INITIALIZED.load(Ordering::Acquire)
        {
            let ppn = super::PhysAddr(kernel_vaddr_to_paddr(addr)).floor();
            if super::page_meta::meta_of(ppn).get_owner()
                == super::page_meta::FrameOwner::User
            {
                super::frame::dealloc_frames(ppn, large_order(size, layout.align()));
                return;
            }
        }
"""
new="""        // 2. SLUB 分配的对象：优先读 MetaSlot allocator cookie（O(1)，
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
"""
assert old in s
s=s.replace(old,new,1)
p.write_text(s)
print('alloc cookie patch ok')
