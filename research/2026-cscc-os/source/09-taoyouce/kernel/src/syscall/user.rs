//! 用户空间内存访问 helper
//!
//! 当前实现保持与之前 inline 在 `syscall/mod.rs` 中的语义完全一致：
//! - `read_user_buffer` / `read_c_string` / `read_user_usize` / `write_user_buffer`
//!   使用 `Result<_, &'static str>` 错误形式，供低级 syscall 路径使用
//! - `read_user_string` 使用 `Result<_, SyscallError>`，供 exec 等高层路径使用
//!
//! 后续按 `kernel-architecture-rebuild-implementation-plan.md` Milestone 2 演进
//! 为 UserPtr / UserSlice / UserCString 类型化包装，并统一错误模型。

use alloc::string::String;
use alloc::vec::Vec;

use super::errno::SyscallError;
use super::ppn_offset_to_kernel_ptr;

#[derive(Clone, Copy)]
pub(in crate::syscall) struct UserPtr<T> {
    ptr: *mut T,
}

impl<T> UserPtr<T> {
    pub(in crate::syscall) fn new(ptr: *mut T) -> Self {
        Self { ptr }
    }

    pub(in crate::syscall) fn from_addr(addr: usize) -> Self {
        Self {
            ptr: addr as *mut T,
        }
    }

    pub(in crate::syscall) fn is_null(self) -> bool {
        self.ptr.is_null()
    }

    pub(in crate::syscall) fn as_mut_u8_ptr(self) -> *mut u8 {
        self.ptr as *mut u8
    }

    pub(in crate::syscall) fn as_u8_ptr(self) -> *const u8 {
        self.ptr as *const u8
    }
}

#[derive(Clone, Copy)]
pub(crate) struct UserSlice<T> {
    ptr: UserPtr<T>,
    len: usize,
}

impl<T> UserSlice<T> {
    pub(crate) fn new(ptr: *const T, len: usize) -> Self {
        Self {
            ptr: UserPtr::new(ptr as *mut T),
            len,
        }
    }

    pub(crate) fn new_mut(ptr: *mut T, len: usize) -> Self {
        Self {
            ptr: UserPtr::new(ptr),
            len,
        }
    }

    pub(crate) fn read_bytes(self) -> Result<Vec<u8>, SyscallError> {
        read_user_buffer(self.ptr.as_u8_ptr(), self.len).map_err(|_| SyscallError::BadAddress)
    }

    pub(crate) fn write_bytes(self, data: &[u8]) -> Result<(), SyscallError> {
        write_user_buffer(self.ptr.as_mut_u8_ptr(), data).map_err(|_| SyscallError::BadAddress)
    }
}

#[derive(Clone, Copy)]
pub(in crate::syscall) struct UserCString {
    ptr: UserPtr<u8>,
}

impl UserCString {
    pub(in crate::syscall) fn new(ptr: *const u8) -> Self {
        Self {
            ptr: UserPtr::new(ptr as *mut u8),
        }
    }

    pub(in crate::syscall) fn read(self) -> Result<String, SyscallError> {
        read_c_string(self.ptr.as_u8_ptr()).map_err(|_| SyscallError::BadAddress)
    }
}

/// 取当前进程地址空间的 `MmStruct` 句柄（线程回退到 tgid leader 的 mm）。
///
/// 快路径：经 `HART_CURRENT_PCB`（smp=1、syscall 全程中断关闭，见其安全契约）
/// 免锁直取当前 PCB 的 `mm` Arc（CLONE_VM 线程共享同一 Arc，天然覆盖
/// leader 回退）。慢路径（PCB 指针为空或 mm 为 None 的 embryo/内核线程）
/// 回退到持 PROCESS_TABLE 锁的 tgid-leader 查找。
#[inline]
pub(crate) fn current_mm(
) -> Result<alloc::sync::Arc<crate::mm::mm_struct::MmStruct>, &'static str> {
    use crate::sched::process::PROCESS_TABLE;
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        if let Some(mm) = p.mm.as_ref() {
            return Ok(mm.clone());
        }
    }
    let table_guard = PROCESS_TABLE.lock();
    let table = table_guard
        .as_ref()
        .ok_or("Process table not initialized")?;
    let current_pid = table.current.ok_or("No current process")?;
    let as_pid = {
        let p = table.get(current_pid).ok_or("Current process not found")?;
        if p.mm.is_some() { current_pid } else { p.tgid }
    };
    let target = table.get(as_pid).ok_or("Leader process not found")?;
    target.mm.as_ref().cloned().ok_or("No mm")
}

/// 从用户空间读取一段缓冲区
///
/// # 参数
/// - `user_ptr`: 用户空间指针
/// - `len`: 要读取的字节数
///
/// # 返回
/// - `Ok(Vec<u8>)`: 读取成功，返回数据
/// - `Err(&'static str)`: 读取失败
pub(crate) fn read_user_buffer(user_ptr: *const u8, len: usize) -> Result<Vec<u8>, &'static str> {
    use crate::mm::{PhysPageNum, VirtAddr, VirtPageNum, PAGE_SIZE};
    use crate::sched::process::PROCESS_TABLE;

    if len == 0 {
        return Ok(Vec::new());
    }

    if len > 1024 * 1024 {
        return Err("Buffer too large");
    }

    let user_addr = user_ptr as usize;
    let end_addr = user_addr.checked_add(len).ok_or("User address overflow")?;

    // 计算需要访问的页面范围
    let start_va = VirtAddr(user_addr);
    let end_va = VirtAddr(end_addr);
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();

    // 单页快路径：sigset/timespec/futex 字等小对象的读取只跨一页，
    // 免去 mappings Vec 的堆分配，只做一次翻译 + 一次拷贝。
    if end_vpn.0 - start_vpn.0 == 1 {
        let ppn = {
            let mm = current_mm()?;
            let mut address_space = mm.lock();
            match address_space.page_table().translate_readable(start_vpn) {
                Some(ppn) => ppn,
                None => {
                    if address_space.page_table().translate(start_vpn).is_some() {
                        return Err("User address not mapped");
                    }
                    if address_space.handle_page_fault(start_va, false) {
                        address_space
                            .page_table()
                            .translate_readable(start_vpn)
                            .ok_or("User address not mapped")?
                    } else {
                        return Err("User address not mapped");
                    }
                }
            }
        };
        let mut buffer: Vec<u8> = Vec::with_capacity(len);
        let src = ppn_offset_to_kernel_ptr(ppn, start_va.page_offset()) as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src, buffer.as_mut_ptr(), len);
            buffer.set_len(len);
        }
        return Ok(buffer);
    }

    // 一次性获取所有需要的页表映射（避免在循环中持有锁）。
    //
    // Linux `copy_from_user` 在访问未建立映射的合法用户页（懒分配的匿名页、
    // 尚未触碰的 mmap/brk 页、CoW 页）时会触发缺页处理把页建立起来，而不是
    // 直接 EFAULT。早期实现只做 `translate`，遇到懒分配页直接报 EFAULT，导致
    // 例如 pipe15 在 la-musl 上写一块 64KiB 未触碰缓冲区时整次 write 失败。
    // 这里改为：translate 失败时调用 `handle_page_fault(va, false)`（读缺页）
    // 按需建立映射后重试，仍失败才视为真正的非法地址 EFAULT。
    let page_mappings: Vec<PhysPageNum> = {
        let mm = current_mm()?;
        let mut address_space = mm.lock();

        let mut mappings = Vec::with_capacity(end_vpn.0 - start_vpn.0);
        for vpn_num in start_vpn.0..end_vpn.0 {
            let vpn = VirtPageNum(vpn_num);
            let ppn = match address_space.page_table().translate_readable(vpn) {
                Some(ppn) => ppn,
                None => {
                    // Check if page is mapped but PROT_NONE (V=1, R=0)
                    if address_space.page_table().translate(vpn).is_some() {
                        return Err("User address not mapped"); // EFAULT: PROT_NONE
                    }
                    let fault_va = VirtAddr(vpn_num * PAGE_SIZE);
                    if address_space.handle_page_fault(fault_va, false) {
                        address_space
                            .page_table()
                            .translate_readable(vpn)
                            .ok_or("User address not mapped")?
                    } else {
                        return Err("User address not mapped");
                    }
                }
            };
            mappings.push(ppn);
        }
        mappings
    }; // 锁在这里释放

    // 分配内核缓冲区并复制数据
    let mut buffer: Vec<u8> = Vec::with_capacity(len);
    unsafe {
        buffer.set_len(len);
    }
    let mut copied = 0usize;

    for (page_idx, ppn) in page_mappings.iter().enumerate() {
        let offset = if page_idx == 0 {
            start_va.page_offset()
        } else {
            0
        };
        let copy_len = (PAGE_SIZE - offset).min(len - copied);
        let src = ppn_offset_to_kernel_ptr(*ppn, offset) as *const u8;
        let dst = unsafe { buffer.as_mut_ptr().add(copied) };
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, copy_len);
        }
        copied += copy_len;
    }

    Ok(buffer)
}

/// Zero-alloc variant of `read_user_buffer`: copies user memory into a
/// caller-provided `&mut [u8]` slice so the hot path avoids any heap
/// allocation.  For buffers spanning ≤ `MAX_STACK_PAGES` pages the page
/// mappings are kept on the stack; larger requests fall back to the
/// Vec-based `read_user_buffer`.
pub(crate) fn read_user_to_slice(user_ptr: *const u8, out: &mut [u8]) -> Result<usize, &'static str> {
    use crate::mm::{PhysPageNum, VirtAddr, VirtPageNum, PAGE_SIZE};
    use crate::sched::process::PROCESS_TABLE;

    let len = out.len();
    if len == 0 {
        return Ok(0);
    }

    let user_addr = user_ptr as usize;
    let end_addr = user_addr.checked_add(len).ok_or("User address overflow")?;

    let start_va = VirtAddr(user_addr);
    let end_va = VirtAddr(end_addr);
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    let num_pages = end_vpn.0 - start_vpn.0;

    const MAX_STACK_PAGES: usize = 4;
    if num_pages > MAX_STACK_PAGES {
        let vec_buf = read_user_buffer(user_ptr, len)?;
        let n = vec_buf.len().min(len);
        out[..n].copy_from_slice(&vec_buf[..n]);
        return Ok(n);
    }

    let mut page_ppns = [PhysPageNum(0); MAX_STACK_PAGES];
    {
        let mm = current_mm()?;
        let mut address_space = mm.lock();

        for (i, vpn_num) in (start_vpn.0..end_vpn.0).enumerate() {
            let vpn = VirtPageNum(vpn_num);
            let ppn = match address_space.page_table().translate_readable(vpn) {
                Some(ppn) => ppn,
                None => {
                    if address_space.page_table().translate(vpn).is_some() {
                        return Err("User address not mapped");
                    }
                    let fault_va = VirtAddr(vpn_num * PAGE_SIZE);
                    if address_space.handle_page_fault(fault_va, false) {
                        address_space.page_table().translate_readable(vpn)
                            .ok_or("User address not mapped")?
                    } else {
                        return Err("User address not mapped");
                    }
                }
            };
            page_ppns[i] = ppn;
        }
    }

    let mut copied = 0usize;
    for (page_idx, ppn) in page_ppns[..num_pages].iter().enumerate() {
        let offset = if page_idx == 0 { start_va.page_offset() } else { 0 };
        let copy_len = (PAGE_SIZE - offset).min(len - copied);
        let src = ppn_offset_to_kernel_ptr(*ppn, offset) as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(copied), copy_len);
        }
        copied += copy_len;
    }

    Ok(copied)
}

/// 从用户空间读取 null-terminated C 字符串
pub(crate) fn read_c_string(user_ptr: *const u8) -> Result<String, &'static str> {
    use crate::mm::{VirtAddr, VirtPageNum};
    use crate::sched::process::PROCESS_TABLE;

    let max_len = 16384usize;
    let user_addr = user_ptr as usize;
    let mut result = Vec::new();

    // 获取当前进程的页表（线程回退到 tgid leader）
    let table_guard = PROCESS_TABLE.lock();
    let table = table_guard
        .as_ref()
        .ok_or("Process table not initialized")?;
    let current_pid = table.current.ok_or("No current process")?;
    let process = table.get(current_pid).ok_or("Current process not found")?;
    let as_pid = if process.mm.is_some() {
        current_pid
    } else {
        process.tgid
    };
    let target = table.get(as_pid).ok_or("Leader process not found")?;
    let mm = target.mm.as_ref().ok_or("No mm")?;
    let mut address_space = mm.lock();

    for i in 0..max_len {
        let va = VirtAddr(user_addr + i);
        let vpn = va.floor();
        let offset = va.page_offset();

        // translate_readable（要求 R+U）而非 translate：用户指针若落入监督态映射
        // （如每个地址空间都存在的内核 1GiB 恒等大页，R=1 但 U=0）必须视为 EFAULT，
        // 否则会把内核物理 RAM 当作 C 字符串读取 → 解析出垃圾路径 → 路径解析卡死
        // （crash02 向 fchownat 等传入落在该大页区间的随机 path 指针 0xafd41bb2）。
        // 未建映射的合法懒页（fork 懒建子 PTE / 懒分配）先走缺页补页再重试。
        let ppn = match address_space.page_table().translate_readable(vpn) {
            Some(ppn) => ppn,
            None => {
                if address_space.page_table().translate(vpn).is_some() {
                    return Err("User address not mapped");
                }
                if address_space.handle_page_fault(va, false) {
                    match address_space.page_table().translate_readable(vpn) {
                        Some(ppn) => ppn,
                        None => return Err("User address not mapped"),
                    }
                } else {
                    return Err("User address not mapped");
                }
            }
        };

        let pa = ppn_offset_to_kernel_ptr(ppn, offset);
        let byte = unsafe { *(pa as *const u8) };
        if byte == 0 {
            return String::from_utf8(result).map_err(|_| "Invalid UTF-8");
        }
        result.push(byte);
    }
    Err("C string too long or not null-terminated")
}

/// 读取一个用户态路径名，返回 Linux `getname()` 等价的 errno。
///
/// 与 `read_c_string` 不同，本函数区分两种长路径错误：
/// - 在前 `PATH_MAX` 字节内遇到未映射页 -> `-EFAULT` (-14)；
/// - 前 `PATH_MAX` 字节全部可读但仍无 NUL 结尾 -> `-ENAMETOOLONG` (-36)，
///   对应 `strncpy_from_user` 拷满 PATH_MAX 后未见终止符的语义。
///
/// 返回 `Ok(path)`（不含 NUL，长度 < PATH_MAX）或 `Err(负 errno)`。
pub(crate) fn read_path(user_ptr: *const u8) -> Result<alloc::string::String, isize> {
    use crate::mm::VirtAddr;
    use crate::sched::process::PROCESS_TABLE;

    const PATH_MAX: usize = 4096;
    let user_addr = user_ptr as usize;
    if user_addr == 0 {
        return Err(-14); // -EFAULT
    }
    let mut result = Vec::new();

    let table_guard = PROCESS_TABLE.lock();
    let table = table_guard.as_ref().ok_or(-14isize)?;
    let current_pid = table.current.ok_or(-14isize)?;
    let process = table.get(current_pid).ok_or(-14isize)?;
    let as_pid = if process.mm.is_some() {
        current_pid
    } else {
        process.tgid
    };
    let target = table.get(as_pid).ok_or(-14isize)?;
    let mm = target.mm.as_ref().ok_or(-14isize)?;
    let mut address_space = mm.lock();

    // 按页处理（对标 Linux strncpy_from_user）：每页只做一次页表翻译，
    // 页内直接扫 NUL，避免逐字节 3 级页表遍历。
    let mut i = 0usize;
    while i < PATH_MAX {
        let va = VirtAddr(user_addr + i);
        let vpn = va.floor();
        let offset = va.page_offset();
        // translate 失败时区分两种情况——
        //   * 页已映射但无 R 权限（PROT_NONE）-> 真 EFAULT；
        //   * 页尚未建立映射（懒分配匿名页 / 未触碰的文件映射页，如 glibc
        //     .rodata 中的 P_tmpdir 字符串）-> 调 handle_page_fault 按需补页后
        //     重试。修复 glibc tmpfile() 传入未触碰 .rodata 路径指针被误判
        //     EFAULT、导致 LA 上 tmpfile() 失败 → 段错误的问题。
        let ppn = match address_space.page_table().translate_readable(vpn) {
            Some(ppn) => ppn,
            None => {
                if address_space.page_table().translate(vpn).is_some() {
                    return Err(-14); // -EFAULT: 已映射但 PROT_NONE
                }
                if address_space.handle_page_fault(va, false) {
                    match address_space.page_table().translate_readable(vpn) {
                        Some(ppn) => ppn,
                        None => return Err(-14),
                    }
                } else {
                    return Err(-14);
                }
            }
        };
        let pa = ppn_offset_to_kernel_ptr(ppn, offset);
        let chunk_len = (crate::mm::PAGE_SIZE - offset).min(PATH_MAX - i);
        let chunk = unsafe { core::slice::from_raw_parts(pa as *const u8, chunk_len) };
        if let Some(pos) = chunk.iter().position(|&b| b == 0) {
            result.extend_from_slice(&chunk[..pos]);
            return alloc::string::String::from_utf8(result).map_err(|_| -14isize);
        }
        result.extend_from_slice(chunk);
        i += chunk_len;
    }
    // Read PATH_MAX readable bytes with no terminator -> name too long.
    Err(-36) // -ENAMETOOLONG
}

/// unlink/rmdir 热路径专用：一次 PROCESS_TABLE 锁同时取出
/// 「路径 + (euid, egid, fs_root=="/")」，替代 read_path + 二次锁表取凭证
/// 的两对 lock/unlock（QEMU TCG 下每对原子都是串行化点，lat_fs remove 热路径）。
/// 拷贝路径期间不持表锁（clone mm Arc 后先放表锁再锁地址空间）。
pub(crate) enum ReadPath<'a> {
    /// 路径全长放进调用方栈缓冲：零堆分配（对标 Linux getname 的
    /// names_cache 定长槽——常见路径 ≪ 一个 slab 槽）。
    Stack(&'a str),
    /// 超出栈缓冲的长路径：溢出到堆（罕见）。
    Heap(alloc::string::String),
}

impl<'a> ReadPath<'a> {
    #[inline]
    pub(crate) fn as_str(&self) -> &str {
        match self {
            ReadPath::Stack(s) => s,
            ReadPath::Heap(s) => s.as_str(),
        }
    }
}

/// 当前进程的 fs 结构（cwd/root）：经 HART_CURRENT_PCB 免锁快路径，
/// 同 current_mm 安全契约；仅供路径解析慢路径（非规范/相对路径）使用，
/// 热路径（规范绝对路径）不再付 Arc 克隆/析构。
pub(crate) fn current_fs(
) -> Result<alloc::sync::Arc<crate::sync::SpinLock<crate::sched::process::FsStruct>>, isize> {
    use crate::sched::process::PROCESS_TABLE;
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        return Ok(unsafe { &*pcb_ptr }.fs.clone());
    }
    let table_guard = PROCESS_TABLE.lock();
    let table = table_guard.as_ref().ok_or(-14isize)?;
    let current_pid = table.current.ok_or(-14isize)?;
    Ok(table.get(current_pid).ok_or(-14isize)?.fs.clone())
}

/// 边扫边拷：把 `src` 拷进 `dst`（`dst.len() >= src.len()`），word-at-a-time
/// 同时完成 NUL 定位与非 ASCII 探测，一遍替代「position 扫 NUL +
/// memmove 拷贝 + is_ascii 重扫」三遍。命中 NUL 时整字写入（NUL 后尾
/// 字节为同页已读字节，调用方只消费 `..pos`）；非 ASCII 判定只计入
/// NUL 之前的字节。返回 (NUL 位置, 是否见非 ASCII 字节)。
#[inline]
fn copy_scan(dst: &mut [core::mem::MaybeUninit<u8>], src: &[u8]) -> (Option<usize>, bool) {
    const HI: u64 = 0x8080_8080_8080_8080;
    const LO: u64 = 0x0101_0101_0101_0101;
    let n = src.len();
    debug_assert!(dst.len() >= n);
    let mut nonascii: u64 = 0;
    let mut i = 0usize;
    while i + 8 <= n {
        let v = unsafe { core::ptr::read_unaligned(src.as_ptr().add(i) as *const u64) };
        unsafe { core::ptr::write_unaligned(dst.as_mut_ptr().add(i) as *mut u64, v) };
        let zero = v.wrapping_sub(LO) & !v & HI;
        if zero != 0 {
            // 小端：最低置位字节 = 第一个 NUL。
            let byte = (zero.trailing_zeros() as usize) >> 3;
            let before = if byte == 0 {
                0
            } else {
                v & (u64::MAX >> (64 - byte * 8))
            };
            nonascii |= before;
            return (Some(i + byte), nonascii & HI != 0);
        }
        nonascii |= v;
        i += 8;
    }
    while i < n {
        let b = src[i];
        dst[i].write(b);
        if b == 0 {
            return (Some(i), nonascii & HI != 0);
        }
        nonascii |= b as u64;
        i += 1;
    }
    (None, nonascii & HI != 0)
}

pub(crate) fn read_path_and_cred<'a>(
    user_ptr: *const u8,
    buf: &'a mut [core::mem::MaybeUninit<u8>],
) -> Result<(ReadPath<'a>, u32, u32, bool), isize> {
    use crate::mm::VirtAddr;
    use crate::sched::process::PROCESS_TABLE;

    const PATH_MAX: usize = 4096;
    let user_addr = user_ptr as usize;
    if user_addr == 0 {
        return Err(-14); // -EFAULT
    }

    // 免锁快路径：经 HART_CURRENT_PCB（smp=1、syscall 全程中断关闭，
    // 见其安全契约）直取当前 PCB 的 cred/mm，同 current_mm；且 mm 只
    // 借用不克隆 Arc（syscall 全程 PCB 存活、mm 不可能被并发替换）。
    // 慢路径（PCB 空或 mm 为 None 的线程）回退持 PROCESS_TABLE 锁。
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    let mut mm_owned: Option<alloc::sync::Arc<crate::mm::mm_struct::MmStruct>> = None;
    let (euid, egid, root_is_plain, mm): (u32, u32, bool, &crate::mm::mm_struct::MmStruct) = 'cred: {
        if !pcb_ptr.is_null() {
            let p = unsafe { &*pcb_ptr };
            if let Some(mm) = p.mm.as_ref() {
                break 'cred (p.euid, p.egid, p.fs_root_is_plain(), mm);
            }
        }
        let table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_ref().ok_or(-14isize)?;
        let current_pid = table.current.ok_or(-14isize)?;
        let process = table.get(current_pid).ok_or(-14isize)?;
        let as_pid = if process.mm.is_some() {
            current_pid
        } else {
            process.tgid
        };
        let target = table.get(as_pid).ok_or(-14isize)?;
        let euid = process.euid;
        let egid = process.egid;
        let root_is_plain = process.fs_root_is_plain();
        mm_owned = Some(target.mm.as_ref().ok_or(-14isize)?.clone());
        drop(table_guard);
        (euid, egid, root_is_plain, mm_owned.as_deref().unwrap())
    };

    let mut address_space = mm.lock();
    // 先填调用方栈缓冲，仅当路径长度超出缓冲时才溢出到堆 Vec。
    // NLL 限制：不能在循环内直接返回借自 buf 的 &str（后续迭代还要写
    // buf），故循环只产出终态（栈内长度 / 堆 Vec），循环后再构造引用。
    enum Outcome {
        Stack(usize),
        Heap(Vec<u8>),
    }
    let mut spill: Option<Vec<u8>> = None;
    let mut filled = 0usize;
    let mut i = 0usize;
    let mut nonascii = false;
    let outcome = loop {
        if i >= PATH_MAX {
            return Err(-36); // -ENAMETOOLONG
        }
        let va = VirtAddr(user_addr + i);
        let vpn = va.floor();
        let offset = va.page_offset();
        let ppn = match address_space.page_table().translate_readable(vpn) {
            Some(ppn) => ppn,
            None => {
                if address_space.page_table().translate(vpn).is_some() {
                    return Err(-14); // -EFAULT: 已映射但 PROT_NONE
                }
                if address_space.handle_page_fault(va, false) {
                    match address_space.page_table().translate_readable(vpn) {
                        Some(ppn) => ppn,
                        None => return Err(-14),
                    }
                } else {
                    return Err(-14);
                }
            }
        };
        let pa = ppn_offset_to_kernel_ptr(ppn, offset);
        let chunk_len = (crate::mm::PAGE_SIZE - offset).min(PATH_MAX - i);
        let chunk = unsafe { core::slice::from_raw_parts(pa as *const u8, chunk_len) };
        if spill.is_none() {
            // 常规路径：先在栈缓冲剩余空间内边扫边拷（一遍完成拷贝 +
            // NUL 定位 + 非 ASCII 探测）；NUL 落在空间内即零堆分配收束，
            // 超长路径才把余下字节溢出到堆。
            let space = buf.len() - filled;
            let scan_len = chunk_len.min(space);
            let (nul, na) = copy_scan(&mut buf[filled..filled + scan_len], &chunk[..scan_len]);
            // buf[..filled+scan_len(或 pos)] 至此已全部写入。
            nonascii |= na;
            if let Some(pos) = nul {
                break Outcome::Stack(filled + pos);
            }
            filled += scan_len;
            if scan_len < chunk_len {
                // 安全：buf[..filled] 已由 copy_scan 全部写入。
                let init: &[u8] = unsafe {
                    core::slice::from_raw_parts(buf.as_ptr() as *const u8, filled)
                };
                let rest = &chunk[scan_len..];
                if let Some(pos) = rest.iter().position(|&b| b == 0) {
                    let mut v = Vec::with_capacity(filled + pos);
                    v.extend_from_slice(init);
                    v.extend_from_slice(&rest[..pos]);
                    break Outcome::Heap(v);
                }
                let mut v = Vec::with_capacity(filled + rest.len() + 64);
                v.extend_from_slice(init);
                v.extend_from_slice(rest);
                spill = Some(v);
            }
        } else {
            if let Some(pos) = chunk.iter().position(|&b| b == 0) {
                let mut v = spill.take().unwrap();
                v.extend_from_slice(&chunk[..pos]);
                break Outcome::Heap(v);
            }
            if let Some(v) = &mut spill {
                v.extend_from_slice(chunk);
            }
        }
        i += chunk_len;
    };
    drop(address_space);
    drop(mm_owned);
    match outcome {
        Outcome::Stack(total) => {
            // 安全：buf[..total] 已由 copy_scan 全部写入。
            let bytes: &[u8] =
                unsafe { core::slice::from_raw_parts(buf.as_ptr() as *const u8, total) };
            // 路径几乎总是纯 ASCII：copy_scan 已在拷贝时顺带探测高位，
            // 免重扫；非 ASCII 罕见路径才走 from_utf8 完整校验。
            let s = if !nonascii {
                unsafe { core::str::from_utf8_unchecked(bytes) }
            } else {
                core::str::from_utf8(bytes).map_err(|_| -14isize)?
            };
            Ok((ReadPath::Stack(s), euid, egid, root_is_plain))
        }
        Outcome::Heap(v) => {
            let s = if v.is_ascii() {
                unsafe { alloc::string::String::from_utf8_unchecked(v) }
            } else {
                alloc::string::String::from_utf8(v).map_err(|_| -14isize)?
            };
            Ok((ReadPath::Heap(s), euid, egid, root_is_plain))
        }
    }
}

/// 从用户空间读取一个 usize（8 字节，小端）
pub(in crate::syscall) fn read_user_usize(user_addr: usize) -> Result<usize, &'static str> {
    let bytes = read_user_buffer(user_addr as *const u8, 8)?;
    Ok(usize::from_le_bytes(bytes[..8].try_into().unwrap()))
}

/// 从用户空间读取以 null 结尾的字符串
///
/// 通过页表翻译安全地访问用户空间内存
pub(in crate::syscall) fn read_user_string(ptr: *const u8) -> Result<String, SyscallError> {
    use crate::mm::{VirtAddr, VirtPageNum};
    use crate::sched::process::PROCESS_TABLE;

    const MAX_PATH_LEN: usize = 4096;
    let mut result = Vec::new();
    let user_addr = ptr as usize;

    // 获取当前进程的页表（线程回退到 tgid leader）
    {
        let table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_ref().ok_or(SyscallError::InvalidArgument)?;
        let current_pid = table.current.ok_or(SyscallError::InvalidArgument)?;
        let process = table
            .get(current_pid)
            .ok_or(SyscallError::InvalidArgument)?;
        let as_pid = if process.mm.is_some() {
            current_pid
        } else {
            process.tgid
        };
        let mm = table.get(as_pid)
            .and_then(|p| p.mm.as_ref())
            .ok_or(SyscallError::InvalidArgument)?
            .clone();
        let mut address_space = mm.lock();

        // 按页处理：每页一次翻译，页内扫 NUL（同 read_path）。
        // 未建映射的合法懒页（fork 懒建子 PTE / 懒分配）先走缺页补页再重试。
        let mut i = 0usize;
        while i < MAX_PATH_LEN {
            let va = VirtAddr(user_addr + i);
            let vpn = va.floor();
            let offset = va.page_offset();

            // 通过页表翻译虚拟地址到物理地址。与 read_c_string/read_path 同款
            // 加固（crash02 教训）：必须 R+U 双校验——用户指针落入监督态映射
            // （RV 每个地址空间都挂载的内核恒等大页，R=1 但 U=0）时必须 EFAULT，
            // 否则会把内核物理 RAM 当 C 字符串读出（信息泄漏/解析卡死）。
            let ppn = match address_space.page_table().translate_readable(vpn) {
                Some(ppn) => ppn,
                None => {
                    if address_space.page_table().translate(vpn).is_some() {
                        return Err(SyscallError::InvalidArgument); // 已映射但无 R/U
                    }
                    if address_space.handle_page_fault(va, false) {
                        address_space
                            .page_table()
                            .translate_readable(vpn)
                            .ok_or(SyscallError::InvalidArgument)?
                    } else {
                        return Err(SyscallError::InvalidArgument);
                    }
                }
            };

            let kva = ppn_offset_to_kernel_ptr(ppn, offset);
            let chunk_len = (crate::mm::PAGE_SIZE - offset).min(MAX_PATH_LEN - i);
            let chunk = unsafe { core::slice::from_raw_parts(kva as *const u8, chunk_len) };
            match chunk.iter().position(|&b| b == 0) {
                Some(pos) => {
                    result.extend_from_slice(&chunk[..pos]);
                    break;
                }
                None => {
                    result.extend_from_slice(chunk);
                    i += chunk_len;
                }
            }
        }
    } // table_guard + address_space locks released here

    String::from_utf8(result).map_err(|_| SyscallError::InvalidArgument)
}

/// 向用户空间写入一段缓冲区
///
/// 遇到只读页会调用 CoW fault handler 尝试转换为可写副本。
pub(crate) fn write_user_buffer(user_ptr: *mut u8, data: &[u8]) -> Result<(), &'static str> {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    use crate::sched::process::PROCESS_TABLE;

    if data.is_empty() {
        return Ok(());
    }

    let user_addr = user_ptr as usize;
    user_addr
        .checked_add(data.len())
        .ok_or("User address overflow")?;

    let mm = current_mm()?;
    let mut address_space = mm.lock();

    let mut copied = 0usize;
    while copied < data.len() {
        let va = VirtAddr(user_addr + copied);
        let vpn = va.floor();
        let offset = va.page_offset();
        let copy_len = (PAGE_SIZE - offset).min(data.len() - copied);

        // 单次走表：页已可写（常见情形）直接拿 ppn；否则按未映射/CoW 走缺页。
        let ppn = match address_space.page_table().translate_writable(vpn) {
            Some(ppn) => ppn,
            None => {
                if !address_space.handle_page_fault(va, true) {
                    if address_space.page_table().translate(vpn).is_some() {
                        return Err("CoW fault handling failed");
                    }
                    return Err("User address not mapped");
                }
                address_space
                    .page_table()
                    .translate(vpn)
                    .ok_or("User address not mapped after CoW")?
            }
        };

        let dst = ppn_offset_to_kernel_ptr(ppn, offset) as *mut u8;
        let src = unsafe { data.as_ptr().add(copied) };
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, copy_len);
        }
        copied += copy_len;
    }

    Ok(())
}

/// 把用户缓冲区 `[user_ptr, user_ptr+len)` 解析成一组内核直通可写段
/// `(内核指针, 段长)`（每段不跨页）。与 `write_user_buffer` 相同的
/// CoW/缺页处理，但只返回映射不做拷贝，供零拷贝路径（如 pipe 读侧
/// 直接把环形缓冲写进用户页）使用。
pub(crate) fn user_writable_segments(
    user_ptr: *mut u8,
    len: usize,
) -> Result<Vec<(*mut u8, usize)>, &'static str> {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    use crate::sched::process::PROCESS_TABLE;

    if len == 0 {
        return Ok(Vec::new());
    }
    let user_addr = user_ptr as usize;
    user_addr.checked_add(len).ok_or("User address overflow")?;

    let mm = current_mm()?;
    let mut address_space = mm.lock();

    let mut segs = Vec::with_capacity(len / PAGE_SIZE + 2);
    let mut covered = 0usize;
    while covered < len {
        let va = VirtAddr(user_addr + covered);
        let vpn = va.floor();
        let offset = va.page_offset();
        let seg_len = (PAGE_SIZE - offset).min(len - covered);

        // 单次走表：页已可写（常见情形）直接拿 ppn；否则按未映射/CoW 走缺页。
        let ppn = match address_space.page_table().translate_writable(vpn) {
            Some(ppn) => ppn,
            None => {
                if !address_space.handle_page_fault(va, true) {
                    if address_space.page_table().translate(vpn).is_some() {
                        return Err("CoW fault handling failed");
                    }
                    return Err("User address not mapped");
                }
                address_space
                    .page_table()
                    .translate(vpn)
                    .ok_or("User address not mapped after CoW")?
            }
        };
        segs.push((ppn_offset_to_kernel_ptr(ppn, offset) as *mut u8, seg_len));
        covered += seg_len;
    }
    Ok(segs)
}

/// 把用户缓冲区 `[user_ptr, user_ptr+len)` 解析成一组内核直通只读段
/// `(内核指针, 段长)`（每段不跨页）。与 `read_user_buffer` 相同的缺页
/// 处理，但只返回映射不做拷贝，供零拷贝写路径（Regular 文件 write 直接
/// 从用户页搬进文件页缓存，省一次 1MiB 级 staging 拷贝+堆分配）使用。
pub(crate) fn user_readable_segments(
    user_ptr: *const u8,
    len: usize,
) -> Result<Vec<(*const u8, usize)>, &'static str> {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    use crate::sched::process::PROCESS_TABLE;

    if len == 0 {
        return Ok(Vec::new());
    }
    let user_addr = user_ptr as usize;
    user_addr.checked_add(len).ok_or("User address overflow")?;

    let mm = current_mm()?;
    let mut address_space = mm.lock();

    let mut segs = Vec::with_capacity(len / PAGE_SIZE + 2);
    let mut covered = 0usize;
    while covered < len {
        let va = VirtAddr(user_addr + covered);
        let vpn = va.floor();
        let offset = va.page_offset();
        let seg_len = (PAGE_SIZE - offset).min(len - covered);

        // R+U 双校验（crash02 同款加固）：用户指针落入监督态映射（RV 内核
        // 恒等大页）必须 EFAULT，绝不能把内核 RAM 当用户数据段读进文件
        // （write 路径信息泄漏）。PROT_NONE（V=1 无 R）同样 EFAULT。
        match address_space.page_table().translate_readable(vpn) {
            Some(ppn) => {
                segs.push((ppn_offset_to_kernel_ptr(ppn, offset) as *const u8, seg_len));
                covered += seg_len;
                continue;
            }
            None => {
                if address_space.page_table().translate(vpn).is_some() {
                    return Err("User address not mapped"); // 已映射但无 R/U
                }
            }
        }
        if !address_space.handle_page_fault(va, false) {
            return Err("User address not mapped");
        }
        let ppn = match address_space.page_table().translate_readable(vpn) {
            Some(ppn) => ppn,
            None => {
                if address_space.page_table().translate(vpn).is_some() {
                    return Err("User address not mapped");
                }
                return Err("User address not mapped after fault");
            }
        };
        segs.push((ppn_offset_to_kernel_ptr(ppn, offset) as *const u8, seg_len));
        covered += seg_len;
    }
    Ok(segs)
}

/// FOLL_FORCE 写当前进程地址空间（供 `/proc/self/mem` 写入）。与 `write_user_buffer`
/// 的区别：目标页所在 VMA 即便不可写（PROT_READ）也强制写入——破 COW 后写私有副本，
/// 不改 VMA 权限、不污染其它映射（thp04 写只读 THP）。越界/未映射/OOM 返回 Err。
pub(crate) fn force_write_user_buffer(user_ptr: *mut u8, data: &[u8]) -> Result<(), &'static str> {
    use crate::mm::VirtAddr;
    use crate::sched::process::PROCESS_TABLE;

    if data.is_empty() {
        return Ok(());
    }
    let user_addr = user_ptr as usize;
    user_addr
        .checked_add(data.len())
        .ok_or("User address overflow")?;

    let mut table_guard = PROCESS_TABLE.lock();
    let table = table_guard
        .as_mut()
        .ok_or("Process table not initialized")?;
    let current_pid = table.current.ok_or("No current process")?;
    let as_pid = {
        let p = table.get(current_pid).ok_or("Current process not found")?;
        if p.mm.is_some() { current_pid } else { p.tgid }
    };
    let process = table.get(as_pid).ok_or("Leader process not found")?;
    let mm = process.mm.as_ref().ok_or("No mm")?;
    let mm_arc = mm.clone();
    let mut address_space = mm.lock();
    let ok = address_space.force_write(VirtAddr(user_addr), data);
    // force_write 的 COW 拆页把旧帧放入 quarantine，并只异步登记跨核 TLB
    // 失效。必须在本 syscall 返回前同步 drain + 回收：否则其它 hart 可经
    // 陈旧 TLB 读到写前旧页（/proc/self/mem 写对同进程多线程不可见），
    // 旧帧也滞留 quarantine（每字节强写泄漏一页）。
    drop(address_space);
    drop(table_guard);
    crate::mm::tlb::drain_shootdown();
    mm_arc.lock().reap_retired_frames();
    if ok {
        Ok(())
    } else {
        Err("force_write failed")
    }
}

/// Check if a user-space address range is plausibly valid (non-zero, within user range).
/// This is a fast pre-check; actual faults are caught by write_user_buffer/read_user_buffer.
pub(crate) fn is_user_addr_valid(addr: usize, len: usize) -> bool {
    if addr == 0 || len == 0 {
        return false;
    }
    // User space is below kernel space boundary (typically 0x0000_003F_FFFF_FFFF for Sv39)
    let end = addr.saturating_add(len);
    end > addr // no overflow
}
