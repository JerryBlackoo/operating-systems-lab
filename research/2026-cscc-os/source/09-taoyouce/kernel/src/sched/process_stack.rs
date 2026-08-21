use crate::mm::{alloc_frames_uninit, dealloc_frames, PhysPageNum, PAGE_SIZE};
use crate::mm::page_meta::{self, FrameOwner};
use crate::sync::SpinLock;

/// 内核栈回收池（对标 Linux `free_thread_stack` 的 per-cpu cached_stacks）。
/// 只缓存默认阶（DEFAULT_KERNEL_STACK_PAGES）的栈块：Drop 时不归还 buddy、
/// MetaSlot 保持 owner=KernelStack 原样进池，`new` 命中时零 meta/buddy 开销
/// 直接复用。池满（上限 32 = 2MB）时照旧走 dealloc_frames 完整释放。
const STACK_POOL_CAP: usize = 32;
static STACK_POOL: SpinLock<StackPool> = SpinLock::new(StackPool { slots: [PhysPageNum(0); STACK_POOL_CAP], len: 0 });

struct StackPool {
    slots: [PhysPageNum; STACK_POOL_CAP],
    len: usize,
}

/// 进程内核栈
#[derive(Debug)]
pub struct KernelStack {
    /// 栈底物理页号（用于释放）
    ppn: PhysPageNum,
    /// 栈底虚拟地址
    pub bottom: usize,
    /// 栈大小（字节）
    pub size: usize,
}

impl KernelStack {
    /// 分配新的内核栈
    ///
    /// 通过 `alloc_frames(order)` 从 buddy 分配连续物理帧。`alloc_frames` 内部
    /// 已将每帧 MetaSlot 初始化为 (ref_count=1, owner=User)。此处追加：
    /// - 将 owner 改为 `KernelStack`（运行时标记，配合 MetaSlot 统计和防御检查）
    /// - 在 head 帧设置 compound order
    pub fn new(pages: usize) -> Option<Self> {
        let order = pages.next_power_of_two().trailing_zeros() as usize;
        let size = pages * PAGE_SIZE;
        // 池命中：MetaSlot 仍是完整初始化的 KernelStack 块，直接复用。
        if pages == super::process::DEFAULT_KERNEL_STACK_PAGES {
            let recycled = {
                let mut pool = STACK_POOL.lock();
                if pool.len > 0 {
                    pool.len -= 1;
                    Some(pool.slots[pool.len])
                } else {
                    None
                }
            };
            if let Some(ppn) = recycled {
                #[cfg(target_arch = "loongarch64")]
                let bottom = crate::mm::phys_to_kernel_vaddr(ppn.0 << 12);
                #[cfg(not(target_arch = "loongarch64"))]
                let bottom = ppn.0 << 12;
                return Some(Self { ppn, bottom, size });
            }
        }
        // 不清零：内核栈内容先写后读，TrapContext 由 fork/exec 路径完整覆写，
        // 省掉每次 fork 64KB memset。
        let ppn = alloc_frames_uninit(order)?;

        // 将 alloc_frames 设定的 owner=User 改为 KernelStack（精确标记用途）
        let page_count = 1usize << order;
        for i in 0..page_count {
            let slot = page_meta::meta_of(PhysPageNum(ppn.0 + i));
            slot.set_owner(FrameOwner::KernelStack);
            if i == 0 {
                slot.set_order(order as u8);
            }
        }

        #[cfg(target_arch = "loongarch64")]
        {
            let paddr = ppn.0 << 12;
            let bottom = crate::mm::phys_to_kernel_vaddr(paddr);
            Some(Self { ppn, bottom, size })
        }

        #[cfg(not(target_arch = "loongarch64"))]
        {
            let bottom = ppn.0 << 12;
            // 不清零：TrapContext 由 copy_trap_context 完整覆盖，
            // 其余栈先写后读（对齐 LA + Linux）。icount A/B 实证 −2%。
            Some(Self { ppn, bottom, size })
        }
    }

    /// 获取栈顶地址
    pub fn top(&self) -> usize {
        self.bottom + self.size
    }

    /// 获取 TrapContext 应该放置的位置
    pub fn trap_context_position(&self) -> usize {
        self.top() - core::mem::size_of::<crate::arch::TrapContext>()
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn ppn(&self) -> usize {
        self.ppn.0
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        let pages = self.size / PAGE_SIZE;
        if pages == super::process::DEFAULT_KERNEL_STACK_PAGES {
            let mut pool = STACK_POOL.lock();
            if pool.len < STACK_POOL_CAP {
                let len = pool.len;
                pool.slots[len] = self.ppn;
                pool.len = len + 1;
                return;
            }
        }
        let order = pages.next_power_of_two().trailing_zeros() as usize;
        dealloc_frames(self.ppn, order);
    }
}

/// 诊断占位（旧 `track_live_kernel_stack` 已由 MetaSlot owner=KernelStack 替代）。
pub fn track_live_kernel_stack(_pid: usize, _ppn: usize) {}

/// 内核栈底部 canary 魔数，用于检测栈溢出
pub const KSTK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;


