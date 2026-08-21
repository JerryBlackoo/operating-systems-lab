//! clone/fork syscall glue — thread creation and process forking.

use super::process::{CLONE_NEW_MASK, CLONE_NEWPID};
use super::SyscallError;

// ---- clone flags (re-exported for process.rs visibility) ----
pub(in crate::syscall) const CLONE_VM: usize           = 0x0000_0100;
pub(in crate::syscall) const CLONE_VFORK: usize        = 0x0000_4000;
pub(in crate::syscall) const CLONE_FS: usize           = 0x0000_0200;
pub(in crate::syscall) const CLONE_FILES: usize        = 0x0000_0400;
pub(in crate::syscall) const CLONE_SIGHAND: usize      = 0x0000_0800;
pub(in crate::syscall) const CLONE_THREAD: usize       = 0x0001_0000;
pub(in crate::syscall) const CLONE_SETTLS: usize       = 0x0008_0000;
pub(in crate::syscall) const CLONE_PARENT_SETTID: usize= 0x0010_0000;
pub(in crate::syscall) const CLONE_CHILD_CLEARTID: usize=0x0020_0000;
pub(in crate::syscall) const CLONE_CHILD_SETTID: usize = 0x0100_0000;
pub(in crate::syscall) const CLONE_PARENT: usize       = 0x0000_8000;
pub(in crate::syscall) const SIGCHLD_FLAG: usize       = 17;

/// 把子进程 TID 回写到指定 mm 的某用户地址（CLONE_*_SETTID 用）。
/// CoW 页先经写缺页触发私有化，再翻译物理页写入 i32。
fn write_tid_to_mm(
    mm: &alloc::sync::Arc<crate::mm::mm_struct::MmStruct>,
    addr: usize,
    value: i32,
) {
    if addr == 0 {
        return;
    }
    let mut addr_space = mm.lock();
    let va = crate::mm::VirtAddr(addr);
    let vpn = va.floor();
    let offset = va.page_offset();
    let need_fault = match addr_space.page_table().get_pte_flags(vpn) {
        Some(f) => !f.contains(crate::mm::PTEFlags::W),
        None => true,
    };
    if need_fault {
        addr_space.handle_page_fault(va, true);
    }
    if let Some(ppn) = addr_space.page_table().translate(vpn) {
        let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + offset);
        unsafe { core::ptr::write_volatile(kva as *mut i32, value); }
    }
}

/// 安全复制 TrapContext（避免 compiler_builtins memcpy 在 large code model 下的 bug）
#[inline(never)]
unsafe fn copy_trap_context(dst: &mut crate::arch::TrapContext, src: &crate::arch::TrapContext) {
    let dst_base = dst as *mut crate::arch::TrapContext as usize;
    let src_base = src as *const crate::arch::TrapContext as usize;
    let count = core::mem::size_of::<crate::arch::TrapContext>() / 8;

    let mut offset: usize = 0;
    while offset < count {
        let s = core::ptr::read_volatile((src_base + offset * 8) as *const u64);
        core::ptr::write_volatile((dst_base + offset * 8) as *mut u64, s);
        offset += 1;
    }
}

/// clone(flags, child_stack, parent_tidptr, tls, child_tidptr)
///
/// Linux RISC-V clone ABI (nr=220):
///   a0=flags, a1=child_stack, a2=parent_tidptr, a3=tls, a4=child_tidptr
#[allow(dead_code)]
pub(crate) fn sys_clone(flags: usize, child_stack: usize, parent_tidptr: usize, tls: usize, child_tidptr: usize) -> isize {
    sys_clone_impl(flags, child_stack, parent_tidptr, tls, child_tidptr, true)
}

/// clone 主体。`vfork_wait=false` 时 CLONE_VFORK 的父阻塞交给调用方
/// （clone3 需先完成 pidfd/cgroup 写回再等待），此时本函数只创建子并
/// 返回 child_pid；调用方随后必须调用
/// `suspend_vfork_parent_return(child_pid)`。
pub(crate) fn sys_clone_impl(
    flags: usize,
    child_stack: usize,
    parent_tidptr: usize,
    tls: usize,
    child_tidptr: usize,
    vfork_wait: bool,
) -> isize {
    use crate::sched::process::{PROCESS_TABLE, ProcessTable};
    use crate::arch::TrapContext;

    let is_thread = flags & CLONE_THREAD != 0;

    // 创建新命名空间（CLONE_NEWUSER 以外）需要当前 user namespace 内的
    // CAP_SYS_ADMIN：非特权进程 clone(CLONE_NEWUTS/NEWIPC/NEWNS/NEWPID/NEWNET/
    // NEWCGROUP) 必须返 EPERM（utsname04 drop root 后期望 EPERM）。CLONE_NEWUSER
    // 自身不需特权（成功后子进程在新 userns 内获满 caps），故同时携带其它 NEW*
    // 时整体放行——对标 Linux copy_process()→create_user_ns 先于其它命名空间。
    {
        use super::process::{CLONE_NEW_MASK, CLONE_NEWUSER};
        const CAP_SYS_ADMIN: u64 = 1u64 << 21;
        let needs_admin_ns = (flags & CLONE_NEW_MASK & !CLONE_NEWUSER) != 0;
        if needs_admin_ns && (flags & CLONE_NEWUSER) == 0 {
            let privileged = {
                let table = PROCESS_TABLE.lock();
                table.as_ref()
                    .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                    .map(|p| p.euid == 0 || (p.cap_effective & CAP_SYS_ADMIN) != 0)
                    .unwrap_or(false)
            };
            if !privileged {
                return -1; // EPERM
            }
        }
    }

    // 拷贝父进程 TrapContext
    let (parent_ctx_copy, parent_pid) = {
        let table = PROCESS_TABLE.lock();
        if let Some(t) = table.as_ref() {
            if let Some(ppid) = t.current {
                if let Some(parent) = t.get(ppid) {
                    match unsafe { parent.trap_context().cloned() } {
                        Some(ctx) => (ctx, ppid),
                        None => {
                            if crate::cmdline::execdbg_enabled() {
                                crate::println!(
                                    "[EXECDBG] clone flags={:#x} pid={} no-trap-context -> EIO",
                                    flags, ppid
                                );
                            }
                            return SyscallError::IoError as isize;
                        }
                    }
                } else {
                    if crate::cmdline::execdbg_enabled() {
                        crate::println!(
                            "[EXECDBG] clone flags={:#x} current={} missing-pcb -> ENOENT",
                            flags, ppid
                        );
                    }
                    return SyscallError::NotFound as isize;
                }
            } else {
                if crate::cmdline::execdbg_enabled() {
                    crate::println!("[EXECDBG] clone flags={:#x} no-current -> ENOENT", flags);
                }
                return SyscallError::NotFound as isize;
            }
        } else { return SyscallError::IoError as isize; }
    };

    if is_thread {
        // ===== 真线程：共享地址空间 =====
        let mut table_guard = PROCESS_TABLE.lock();
        let table = match table_guard.as_mut() {
            Some(t) => t,
            None => return SyscallError::IoError as isize,
        };
        let child_pid = table.alloc_pid();

        let parent = match table.get(parent_pid) {
            Some(p) => p,
            None => return SyscallError::NotFound as isize,
        };

        let tgid = parent.tgid;
        let parent_page_table_root = parent.page_table_root;
        // Linux copy_fs：CLONE_FS（pthread 默认）共享 fs_struct，否则深拷贝。
        let parent_fs = if flags & CLONE_FS != 0 {
            parent.fs.clone()
        } else {
            alloc::sync::Arc::new(crate::sync::SpinLock::new(parent.fs.lock().clone()))
        };
        let parent_fs_share_id = parent.fs_share_id;
        let parent_nsproxy = parent.nsproxy.clone();
        let parent_name = parent.name.clone();
        let parent_comm = parent.comm.clone();
        let parent_exec_path = parent.exec_path.clone();
        let parent_uid = parent.uid;
        let parent_euid = parent.euid;
        let parent_gid = parent.gid;
        let parent_egid = parent.egid;
        let parent_sgid = parent.sgid;
        let parent_supplementary_groups = parent.supplementary_groups.clone();
        let parent_dumpable = parent.dumpable;
        let parent_no_new_privs = parent.no_new_privs;
        let parent_keepcaps = parent.keepcaps;
        let parent_thp_disable = parent.thp_disable;
        let parent_child_subreaper = parent.child_subreaper;
        let parent_timerslack_ns = parent.timerslack_ns;
        let parent_default_timerslack_ns = parent.timerslack_ns;
        let parent_cap_bset = parent.cap_bset;
        let parent_cap_effective = parent.cap_effective;
        let parent_cap_permitted = parent.cap_permitted;
        let parent_cap_inheritable = parent.cap_inheritable;
        let parent_cap_ambient = parent.cap_ambient;
        let parent_securebits = parent.securebits;
        let parent_seccomp_mode = parent.seccomp_mode;
        let parent_audit_sessionid = parent.audit_sessionid;
        let parent_oom_score_adj = parent.oom_score_adj;
        let parent_saved_auxv = parent.saved_auxv.clone();
        let parent_saved_envp = parent.saved_envp.clone();
        let parent_mm_start_code = parent.mm_start_code;
        let parent_mm_end_code = parent.mm_end_code;
        let parent_mm_start_data = parent.mm_start_data;
        let parent_mm_end_data = parent.mm_end_data;
        let parent_mm_start_brk = parent.mm_start_brk;
        let parent_mm_brk = parent.mm_brk;
        let parent_mm_start_stack = parent.mm_start_stack;
        let parent_mm_arg_start = parent.mm_arg_start;
        let parent_mm_arg_end = parent.mm_arg_end;
        let parent_mm_env_start = parent.mm_env_start;
        let parent_mm_env_end = parent.mm_env_end;
        let parent_heap_start = parent.heap_start;
        let parent_heap_end = parent.heap_end;
        let parent_user_stack_top = parent.user_stack_top;
        let parent_user_stack_bottom = parent.user_stack_bottom;
        let parent_sched_policy = parent.sched_policy;
        let parent_sched_priority = parent.sched_priority;
        let parent_fd_table_owner = parent.fd_table_owner;
        let parent_cgroup_path = parent.cgroup_path.clone();

        // fork 变体：不建 stdio fd 表/nsproxy/默认路径串——这些字段随即全部
        // 被父值覆盖（线程 fd 操作重定向到 fd_table_owner，自身表永不被读）。
        let mut child = crate::sched::process::ProcessControlBlock::new_boxed_for_fork(child_pid);
        child.tgid = tgid;
        child.is_thread = true;
        child.exit_signal = 0; // Linux: CLONE_THREAD → no exit signal
        child.fd_table_owner = parent_fd_table_owner;
        // Linux pids 控制器计数含线程；线程留在父任务的 cgroup 内。
        // 退出路径 mark_zombie 按 cgroup_path uncharge，此处充值保持收支平衡。
        crate::kernel::cgroup::cgroup_pids_charge(&parent_cgroup_path);
        child.cgroup_path = parent_cgroup_path;
        child.startup_credits = crate::sched::process::THREAD_STARTUP_CREDITS;
        child.parent_pid = Some(parent_pid);
        child.name = parent_name;
        child.comm = parent_comm;
        child.exec_path = parent_exec_path;
        child.uid = parent_uid;
        child.euid = parent_euid;
        child.gid = parent_gid;
        child.egid = parent_egid;
        child.sgid = parent_sgid;
        child.supplementary_groups = parent_supplementary_groups;
        child.dumpable = parent_dumpable;
        child.no_new_privs = parent_no_new_privs;
        child.keepcaps = parent_keepcaps;
        child.thp_disable = parent_thp_disable;
        child.child_subreaper = parent_child_subreaper;
        child.timerslack_ns = parent_timerslack_ns;
        child.default_timerslack_ns = parent_default_timerslack_ns;
        child.cap_bset = parent_cap_bset;
        child.cap_effective = parent_cap_effective;
        child.cap_permitted = parent_cap_permitted;
        child.cap_inheritable = parent_cap_inheritable;
        child.cap_ambient = parent_cap_ambient;
        child.securebits = parent_securebits;
        child.seccomp_mode = parent_seccomp_mode;
        child.audit_sessionid = parent_audit_sessionid;
        child.oom_score_adj = parent_oom_score_adj;
        child.saved_auxv = parent_saved_auxv;
        child.saved_envp = parent_saved_envp;
        child.mm_start_code = parent_mm_start_code;
        child.mm_end_code = parent_mm_end_code;
        child.mm_start_data = parent_mm_start_data;
        child.mm_end_data = parent_mm_end_data;
        child.mm_start_brk = parent_mm_start_brk;
        child.mm_brk = parent_mm_brk;
        child.mm_start_stack = parent_mm_start_stack;
        child.mm_arg_start = parent_mm_arg_start;
        child.mm_arg_end = parent_mm_arg_end;
        child.mm_env_start = parent_mm_env_start;
        child.mm_env_end = parent_mm_env_end;
        child.fs = parent_fs;
        if flags & CLONE_FS != 0 {
            child.fs_share_id = parent_fs_share_id;
        }
        // 线程共享父进程的命名空间（Linux: CLONE_THREAD 隐含同 nsproxy）。
        // 不继承会使 cached_mnt_ns_id 停留在 0，线程被调度后 ACTIVE_MNT_NS=0，
        // map_for(0)=None → 所有挂载点(/tmp /proc /dev)不可见，线程内路径解析
        // 走到被覆盖的底层目录（LTP openfile 线程 fopen ENOENT 根因）。
        child.nsproxy = parent_nsproxy;
        child.cached_mnt_ns_id = child.nsproxy.lock().mnt_ns.id.0;
        child.heap_start = parent_heap_start;
        child.heap_end = parent_heap_end;
        child.user_stack_top = parent_user_stack_top;
        child.user_stack_bottom = parent_user_stack_bottom;
        // CLONE_SIGHAND：线程的 sigaction 读者恒走 tgid leader（见 signal.rs /
        // signal_syscall.rs），线程自身的 signal_handlers 数组从不被读，
        // 不拷贝（2KB×2 的 memcpy 是 pthread_create 热路径大头）。
        child.page_table_root = parent_page_table_root;
        // CLONE_VM: 共享父进程地址空间（Arc::clone + mmget）
        if let Some(parent_proc) = table.get(parent_pid) {
            if let Some(ref parent_mm) = parent_proc.mm {
                parent_mm.mmget();
                child.mm = Some(parent_mm.clone());
            }
        }

        if let Err(_e) = child.allocate_kernel_stack() {
            return -12; // -ENOMEM
        }

        let child_ksp = child.kernel_stack_top();
        unsafe {
            if let Some(child_ctx) = child.trap_context_mut() {
                copy_trap_context(child_ctx, &parent_ctx_copy);
                #[cfg(target_arch = "riscv64")]
                { child_ctx.x[10] = 0; }
                #[cfg(target_arch = "loongarch64")]
                { child_ctx.x[4] = 0; }
                child_ctx.kernel_sp = child_ksp;
                if child_stack != 0 {
                    child_ctx.set_sp(child_stack);
                }
                if flags & CLONE_SETTLS != 0 {
                    #[cfg(target_arch = "riscv64")]
                    { child_ctx.x[4] = tls; }
                    #[cfg(target_arch = "loongarch64")]
                    { child_ctx.x[2] = tls; }
                }
                // 新线程早期以 TP 为基址读 pthread 自身结构（`BADV=0x8` 即
                // `TP=0` 加偏移 8）。这里打印 clone 实参与真正写入的 TP/SP，
                // 用于判定是「libc 未带 CLONE_SETTLS」还是「我们没写对 TP」。
                if crate::cmdline::execdbg_enabled() {
                    #[cfg(target_arch = "loongarch64")]
                    let (tp, sp) = (child_ctx.x[2], child_ctx.x[3]);
                    #[cfg(target_arch = "riscv64")]
                    let (tp, sp) = (child_ctx.x[4], child_ctx.x[2]);
                    crate::println!(
                        "[EXECDBG] clone-thread parent={} child={} flags={:#x} settls={} arg_tls={:#x} arg_stack={:#x} ctx_tp={:#x} ctx_sp={:#x} era={:#x}",
                        parent_pid, child_pid, flags,
                        flags & CLONE_SETTLS != 0, tls, child_stack, tp, sp, child_ctx.sepc
                    );
                }
            }
        }

        // CLONE_PARENT_SETTID
        if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
            let cldbg = crate::cmdline::cldbg_enabled();
            if let Some(parent_proc) = table.get(parent_pid) {
                if let Some(ref mm) = parent_proc.mm {
                    let mut addr_space = mm.lock();
                    let va = crate::mm::VirtAddr(parent_tidptr);
                    let vpn = va.floor();
                    let offset = va.page_offset();
                    let pte_flags = addr_space.page_table().get_pte_flags(vpn);
                    match pte_flags {
                        Some(f) if f.contains(crate::mm::PTEFlags::W) => {}
                        // 只读（CoW）或未建映射（懒页）：先走写缺页补页
                        _ => { addr_space.handle_page_fault(va, true); }
                    }
                    if let Some(ppn) = addr_space.page_table().translate(vpn) {
                        let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + offset);
                        unsafe { core::ptr::write_volatile(kva as *mut i32, child_pid as i32); }
                        if cldbg {
                            let w0 = unsafe { core::ptr::read_volatile(kva as *const i32) };
                            let w8 = unsafe { core::ptr::read_volatile((kva + 8) as *const i32) };
                            crate::println!(
                                "[pcpdbg] settid.ok parent={} child={} ptr={:#x} wrote={} readback={} ptr+8={}",
                                parent_pid, child_pid, parent_tidptr, child_pid, w0, w8
                            );
                        }
                    } else if cldbg {
                        crate::println!(
                            "[pcpdbg] settid.FAIL parent={} child={} ptr={:#x} pte_flags={:?} translate=None",
                            parent_pid, child_pid, parent_tidptr, pte_flags
                        );
                    }
                } else if cldbg {
                    crate::println!("[pcpdbg] settid.FAIL parent={} no mm", parent_pid);
                }
            }
        }

        if flags & CLONE_CHILD_CLEARTID != 0 {
            child.clear_child_tid = child_tidptr;
        }
        if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
            if let Some(parent_proc) = table.get(parent_pid) {
                if let Some(ref mm) = parent_proc.mm {
                    let mut addr_space = mm.lock();
                    let va = crate::mm::VirtAddr(child_tidptr);
                    let vpn = va.floor();
                    let offset = va.page_offset();
                    match addr_space.page_table().get_pte_flags(vpn) {
                        Some(f) if f.contains(crate::mm::PTEFlags::W) => {}
                        // 只读（CoW）或未建映射（懒页）：先走写缺页补页
                        _ => { addr_space.handle_page_fault(va, true); }
                    }
                    if let Some(ppn) = addr_space.page_table().translate(vpn) {
                        let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + offset);
                        unsafe { core::ptr::write_volatile(kva as *mut i32, child_pid as i32); }
                    }
                }
            }
        }

        table.insert_runnable(child_pid, child);
        table.note_startup_candidate(child_pid);

        crate::sched::trace::record_event(
            crate::sched::trace::EV_CLONE_THREAD,
            child_pid,
            tgid,
            parent_sched_policy,
            parent_sched_priority,
            parent_pid as u64,
            flags as u64,
        );

        if let Some(parent) = table.get_mut(parent_pid) {
            parent.set_yield_to_child(child_pid);
        }
        if crate::cmdline::cldbg_enabled()
        {
            crate::println!(
                "[cldbg] clone.thread parent={} child={} tgid={} flags={:#x} child_stack={:#x} tls={:#x} parent_tidptr={:#x} child_tidptr={:#x}",
                parent_pid,
                child_pid,
                tgid,
                flags,
                child_stack,
                tls,
                parent_tidptr,
                child_tidptr
            );
        }

        #[cfg(target_arch = "loongarch64")]
        crate::arch::loongarch64::fpu::fork_inherit(parent_pid, child_pid);
        return child_pid as isize;
    }

    // ===== fork 语义 =====
    let child_sp = if child_stack == 0 { None } else { Some(child_stack) };

    let rc = do_fork_clone(&parent_ctx_copy, flags, child_sp, tls, parent_tidptr, child_tidptr);
    if crate::cmdline::execdbg_enabled() && rc < 0 {
        crate::println!(
            "[EXECDBG] clone flags={:#x} parent={} rc={}",
            flags, parent_pid, rc
        );
    }
    if vfork_wait && flags & CLONE_VFORK != 0 && rc > 0 {
        // 见 suspend_vfork_parent_return 文档：父在 clone 内同步阻塞到子
        // execve/exit；child_pid 已写入返回路径，唤醒后直接恢复用户态。
        crate::sched::process_sched::suspend_vfork_parent_return(rc as usize);
    }
    rc
}

/// fork 路径的独立函数，避免在巨大的 syscall 函数中内联导致栈帧过大
#[inline(never)]
fn do_fork_clone(
    parent_ctx_copy: &crate::arch::TrapContext,
    flags: usize,
    child_sp: Option<usize>,
    tls: usize,
    parent_tidptr: usize,
    child_tidptr: usize,
) -> isize {
    use crate::sched::process::{fork_with_ctx, PROCESS_TABLE};

    match fork_with_ctx(Some(parent_ctx_copy), flags) {
        Ok((child_pid, child_tcp, child_ksp)) => {
            #[cfg(target_arch = "loongarch64")]
            {
                let parent_pid = crate::sched::process_sched::HART_CURRENT_PID
                    .load(core::sync::atomic::Ordering::Relaxed);
                if parent_pid != 0 {
                    crate::arch::loongarch64::fpu::fork_inherit(parent_pid, child_pid);
                }
            }
            #[cfg(not(target_arch = "loongarch64"))]
            unsafe {
                if child_tcp != 0 {
                    let child_ctx = &mut *(child_tcp as *mut crate::arch::TrapContext);
                    if let Some(sp) = child_sp {
                        child_ctx.set_sp(sp);
                    }
                    if flags & CLONE_SETTLS != 0 {
                        child_ctx.x[4] = tls;
                    }
                }
            }

            #[cfg(target_arch = "loongarch64")]
            unsafe {
                if child_tcp != 0 {
                    let child_ctx = &mut *(child_tcp as *mut crate::arch::TrapContext);
                    if let Some(sp) = child_sp {
                        core::ptr::write_volatile(&mut child_ctx.x[3] as *mut usize, sp);
                    }
                    if flags & CLONE_SETTLS != 0 {
                        core::ptr::write_volatile(&mut child_ctx.x[2] as *mut usize, tls);
                    }
                }
            }

            {
                let mut table = PROCESS_TABLE.lock();
                if let Some(table) = table.as_mut() {
                    let shared_fd_owner = if flags & CLONE_FILES != 0 {
                        table
                            .current
                            .and_then(|ppid| table.get(ppid).map(|p| p.fd_table_owner))
                            .unwrap_or(child_pid)
                    } else {
                        child_pid
                    };
                    if let Some(child) = table.get_mut(child_pid) {
                        if flags & CLONE_FILES != 0 {
                            child.discard_fd_table_refs_without_posix();
                            child.fd_table_owner = shared_fd_owner;
                        }
                        if flags & CLONE_CHILD_CLEARTID != 0 {
                            child.clear_child_tid = child_tidptr;
                        }
                        let ns_bits = (flags & CLONE_NEW_MASK) as u64;
                        if ns_bits != 0 {
                            child.clone_ns_flags |= ns_bits;
                        }
                        if flags & CLONE_NEWPID != 0 {
                            child.pidns_root = child.tgid.saturating_sub(1);
                        }
                    }
                    if flags & CLONE_PARENT != 0 {
                        // CLONE_PARENT: the new child's parent is the caller's
                        // parent (its grandparent), not the caller itself.
                        // The exit signal and the right to reap the child then
                        // belong to that grandparent (Linux `copy_process`).
                        if let Some(caller_pid) = table.current {
                            let grandparent = table
                                .get(caller_pid)
                                .and_then(|p| p.parent_pid)
                                .unwrap_or(1);
                            if let Some(caller) = table.get_mut(caller_pid) {
                                caller.children.retain(|&p| p != child_pid);
                            }
                            if let Some(child) = table.get_mut(child_pid) {
                                child.parent_pid = Some(grandparent);
                            }
                            if let Some(gp) = table.get_mut(grandparent) {
                                if !gp.children.contains(&child_pid) {
                                    gp.children.push(child_pid);
                                }
                            }
                        }
                    }
                    // CLONE_PARENT_SETTID / CLONE_CHILD_SETTID tid 回写（fork 路径）。
                    // 线程路径已在上方处理；fork 语义（含 CLONE_VM 非线程）此前完全
                    // 缺失，导致 clone08 的 ctid/ptid 永远读到 0。
                    // 语义对标 Linux copy_process：ptid 写「父进程」内存、ctid 写
                    // 「子进程」内存（CLONE_VM 时父子 mm 同一，写任一皆可）。
                    if flags & (CLONE_PARENT_SETTID | CLONE_CHILD_SETTID) != 0 {
                        let parent_mm = table
                            .current
                            .and_then(|cp| table.get(cp))
                            .and_then(|p| p.mm.clone());
                        let child_mm = table.get(child_pid).and_then(|c| c.mm.clone());
                        if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
                            if let Some(ref mm) = parent_mm {
                                write_tid_to_mm(mm, parent_tidptr, child_pid as i32);
                            }
                        }
                        if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
                            if let Some(ref mm) = child_mm {
                                write_tid_to_mm(mm, child_tidptr, child_pid as i32);
                            }
                        }
                    }
                    table.mark_runnable(child_pid, false);
                    if flags & CLONE_VFORK != 0 {
                        if let Some(parent_pid) = table.current {
                            if let Some(parent) = table.get_mut(parent_pid) {
                                parent.set_yield_to_child(child_pid);
                                parent.vfork_waiting_for = Some(child_pid);
                                // 独立等待态：覆盖父上一次睡眠残留的 IO/POLL
                                // 哨兵，否则 wake_io_waiters 等广播会把 vfork
                                // 父提前唤醒（见 VFORK_WAIT_SENTINEL 文档）。
                                parent.waiting_for =
                                    Some(crate::sched::process::VFORK_WAIT_SENTINEL);
                                parent.wait_channel = Some("vfork_done");
                                parent.waitqueue_woken = false;
                                // 清残留 poll/nanosleep deadline，防 sleep-timer
                                // 到期扫描按旧 wakeup_channel 匹配唤醒 vfork 父。
                                parent.wakeup_channel = 0;
                                parent.state = crate::sched::process::ProcessState::Sleeping;
                            }
                            table.unlink_ready(parent_pid);
                            if crate::cmdline::bscrash_enabled() {
                                crate::println!(
                                    "[VFORK] sleep parent={} child={} flags={:#x}",
                                    parent_pid, child_pid, flags
                                );
                            }
                        }
                    }
                }
            }
            child_pid as isize
        }
        Err(e) => {
            crate::println!("[syscall] fork failed: {}", e);
            if e.starts_with("ENOMEM") {
                SyscallError::OutOfMemory as isize
            } else if e.starts_with("EAGAIN") {
                SyscallError::WouldBlock as isize
            } else {
                SyscallError::NotFound as isize
            }
        }
    }
}
