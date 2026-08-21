use alloc::string::String;
use alloc::vec::Vec;

use crate::mm::{AddressSpace, PAGE_SIZE};
use crate::sched::process::{
    install_user_stack, USER_STACK_PREFAULT_PAGES, USER_STACK_RESERVED_PAGES,
};

use super::error::ExecError;

pub(super) fn allocate_user_stack(address_space: &mut AddressSpace, stack_top: usize) -> Result<(), ExecError> {
    install_user_stack(
        address_space,
        stack_top,
        USER_STACK_RESERVED_PAGES,
        USER_STACK_PREFAULT_PAGES,
    )
    .map(|_| ())
    .map_err(|_| ExecError::OutOfMemory)
}

/// 把 argv/envp/auxv 推到用户栈顶并返回 `(new_sp, saved_auxv)`。
///
/// `saved_auxv` 是写入 `/proc/[pid]/auxv` 的真值（由本函数构造，复用栈上
/// 已写的 AT_RANDOM 地址等）。replace_current_process_address_space 会
/// 把它存进 PCB.saved_auxv，供 procfs `ProcessAuxvGenerator` 直接读。
pub(super) fn push_args_to_stack(
    address_space: &mut AddressSpace,
    stack_top: usize,
    argv: &[String],
    envp: &[String],
    elf_info: &crate::loader::elf::ElfInfo,
) -> Result<(usize, Vec<(u64, u64)>), ExecError> {
    use crate::mm::VirtAddr;

    // argv/envp/auxv are written from `stack_top` downward into the pages the
    // new stack actually prefaults (USER_STACK_PREFAULT_PAGES). The kernel-side
    // writer (`write_bytes`) only translates already-mapped pages, so a total
    // payload larger than that region would fail mid-write. Linux reports such
    // an oversized argument list as E2BIG (not ENOMEM), and LTP thp01 binary-
    // searches the boundary expecting exactly E2BIG. Reject up front so the
    // errno is E2BIG and the write never runs off the prefaulted region.
    {
        const AUXV_PAIRS: usize = 17; // keep in sync with the auxv table below
        let strings: usize = argv.iter().map(|a| a.len() + 1).sum::<usize>()
            + envp.iter().map(|e| e.len() + 1).sum::<usize>();
        let ptr_slots = 1 + argv.len() + 1 + envp.len() + 1 + AUXV_PAIRS * 2;
        let ptr_bytes = ptr_slots * core::mem::size_of::<usize>();
        // 16 bytes AT_RANDOM + generous alignment slack.
        let total = strings + ptr_bytes + 16 + 64;
        let limit = USER_STACK_PREFAULT_PAGES
            .saturating_sub(1)
            * crate::mm::PAGE_SIZE;
        if total > limit {
            return Err(ExecError::ArgListTooLong);
        }
    }

    let page_table = address_space.page_table();
    let mut sp = stack_top;

    // 按页分块写：每页只做一次页表 translate，页内整段 copy（旧实现逐字节
    // 各 translate 一次，三级页表 walk × 每字节，是 exec 画像里的显著热点）。
    let write_bytes = |pt: &crate::mm::PageTable, va: usize, data: &[u8]| -> Result<(), ExecError> {
        let mut written = 0usize;
        while written < data.len() {
            let addr = VirtAddr(va + written);
            let vpn = addr.floor();
            let offset = addr.page_offset();
            let chunk = (PAGE_SIZE - offset).min(data.len() - written);
            let ppn = pt.translate(vpn).ok_or(ExecError::OutOfMemory)?;
            let pa = (ppn.0 << 12) | offset;
            let kernel_va = crate::mm::PhysPageNum::to_kernel_vaddr(pa);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data[written..].as_ptr(),
                    kernel_va as *mut u8,
                    chunk,
                );
            }
            written += chunk;
        }
        Ok(())
    };

    let write_usize = |pt: &crate::mm::PageTable, va: usize, val: usize| -> Result<(), ExecError> {
        let bytes = val.to_le_bytes();
        write_bytes(pt, va, &bytes)
    };

    let mut argv_addrs: Vec<usize> = Vec::new();
    let mut envp_addrs: Vec<usize> = Vec::new();

    for arg in argv.iter() {
        let bytes = arg.as_bytes();
        sp -= bytes.len() + 1;
        write_bytes(page_table, sp, bytes)?;
        write_bytes(page_table, sp + bytes.len(), &[0u8])?;
        argv_addrs.push(sp);
    }

    for env in envp.iter() {
        let bytes = env.as_bytes();
        sp -= bytes.len() + 1;
        write_bytes(page_table, sp, bytes)?;
        write_bytes(page_table, sp + bytes.len(), &[0u8])?;
        envp_addrs.push(sp);
    }

    sp -= 16;
    let random_addr = sp;
    {
        let mut rand_bytes = [0u8; 16];
        crate::random::fill_bytes(&mut rand_bytes);
        write_bytes(page_table, random_addr, &rand_bytes)?;
    }

    sp &= !0xF;

    const AT_NULL: usize = 0;
    const AT_PHDR: usize = 3;
    const AT_PHENT: usize = 4;
    const AT_PHNUM: usize = 5;
    const AT_PAGESZ: usize = 6;
    const AT_BASE: usize = 7;
    const AT_FLAGS: usize = 8;
    const AT_ENTRY: usize = 9;
    const AT_UID: usize = 11;
    const AT_EUID: usize = 12;
    const AT_GID: usize = 13;
    const AT_EGID: usize = 14;
    const AT_SECURE: usize = 23;
    const AT_RANDOM: usize = 25;
    const AT_EXECFN: usize = 31;
    const AT_HWCAP: usize = 16;
    const AT_CLKTCK: usize = 17;

    let execfn_addr = argv_addrs.first().copied().unwrap_or(0);
    let auxv: [(usize, usize); 17] = [
        (AT_PHDR,    elf_info.phdr_vaddr),
        (AT_PHENT,   elf_info.phent_size),
        (AT_PHNUM,   elf_info.phnum),
        (AT_PAGESZ,  PAGE_SIZE),
        (AT_BASE,    elf_info.interp_base),
        (AT_FLAGS,   0),
        (AT_ENTRY,   elf_info.app_entry),
        (AT_UID,     0),
        (AT_EUID,    0),
        (AT_GID,     0),
        (AT_EGID,    0),
        (AT_SECURE,  0),
        (AT_RANDOM,  random_addr),
        (AT_HWCAP,   0),
        (AT_CLKTCK,  100),
        (AT_EXECFN,  execfn_addr),
        (AT_NULL,    0),
    ];

    let auxv_slots = auxv.len() * 2;
    let total_slots = 1 + argv.len() + 1 + envp.len() + 1 + auxv_slots;
    sp -= total_slots * core::mem::size_of::<usize>();
    sp &= !0xF;

    let mut pos = sp;

    write_usize(page_table, pos, argv.len())?;
    pos += core::mem::size_of::<usize>();

    for &addr in argv_addrs.iter() {
        write_usize(page_table, pos, addr)?;
        pos += core::mem::size_of::<usize>();
    }
    write_usize(page_table, pos, 0)?;
    pos += core::mem::size_of::<usize>();

    for &addr in envp_addrs.iter() {
        write_usize(page_table, pos, addr)?;
        pos += core::mem::size_of::<usize>();
    }
    write_usize(page_table, pos, 0)?;
    pos += core::mem::size_of::<usize>();

    for &(aux_type, aux_val) in auxv.iter() {
        write_usize(page_table, pos, aux_type)?;
        pos += core::mem::size_of::<usize>();
        write_usize(page_table, pos, aux_val)?;
        pos += core::mem::size_of::<usize>();
    }

    // Slice 2.5 真实化：把 auxv 转成 (u64,u64) Vec 返回，供 PCB.saved_auxv
    // 写入 / `/proc/[pid]/auxv` 读取。Linux `mm->saved_auxv` 与栈上 auxv
    // 完全同值同序，末尾 (AT_NULL,0) 也含；与 glibc `getauxval(3)` 对齐。
    let saved_auxv: Vec<(u64, u64)> = auxv
        .iter()
        .map(|&(t, v)| (t as u64, v as u64))
        .collect();

    Ok((sp, saved_auxv))
}
