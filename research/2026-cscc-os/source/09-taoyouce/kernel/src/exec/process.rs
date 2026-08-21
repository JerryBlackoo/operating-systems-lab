use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::fd as fd_table;
use crate::loader::ElfInfo;
use crate::mm::{AddressSpace, PAGE_SIZE, PhysAddr, VirtAddr};
use crate::sched::process::{
    DEFAULT_USER_STACK_TOP, PROCESS_TABLE, ProcessState, USER_STACK_RESERVED_PAGES,
    set_process_state,
};

use super::error::ExecError;
use super::trap;

pub(super) fn replace_current_process_address_space(
    new_address_space: AddressSpace,
    elf_info: &ElfInfo,
    name: &str,
    exec_path: &str,
    argv: &[String],
    envp: &[String],
    saved_auxv: Vec<(u64, u64)>,
) -> Result<(), ExecError> {
    let argc = argv.len();
    let envc = envp.len();
    // 1) POSIX execve(2) implicitly closes all FD_CLOEXEC fds. The close must
    //    fire inotify IN_CLOSE_*, lease break, dnotify de-register, and
    //    pipe/socket peer wakeups, all of which re-acquire PROCESS_TABLE.
    //    Collect the displaced fd objects under the lock, then run
    //    `finalize_closed_fd` on each after the lock is dropped.
    // Linux de_thread: execve kills all other threads in the group before the
    // CLOEXEC sweep, so shared-table siblings can't observe their fds vanish.
    crate::sched::process::de_thread_for_exec();
    let (closed_cloexec, exec_pid, detached_shm_ids) = {
        let mut table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_mut().ok_or(ExecError::NoProcess)?;
        let current_pid = table.current.ok_or(ExecError::NoProcess)?;
        let fd_process = table.get_fd_process_mut(current_pid).ok_or(ExecError::NoProcess)?;
        let closed = fd_table::apply_cloexec_on_exec(fd_process);
        // POSIX execve(2): "Any attached System V shared memory segments are
        // detached (shmdt(2))" — see man execve(2)/RATIONALE. Linux 实现路径
        // 是 `flush_old_exec → exit_mm → munmap` 触发 shm_vma_close →
        // shm_close。我们的 SHM 不挂 vm_ops，必须显式收集 attached id 列表，
        // 在锁外触发 nattch-- + maybe_reap，否则 IPC_RMID 的 segment 永远
        // 卡在 nattch>0 不被释放（Linux 真行为是 execve 后立刻 reap）。
        let attached: alloc::vec::Vec<i32> = {
            let proc = table.get_mut(current_pid).ok_or(ExecError::NoProcess)?;
            let ids: alloc::vec::Vec<i32> = proc.shm_attachments.iter().map(|&(id, _)| id).collect();
            proc.shm_attachments.clear();
            ids
        };
        (closed, current_pid, attached)
    };
    for fd_obj in closed_cloexec {
        fd_table::finalize_closed_fd_for_pid(fd_obj, exec_pid);
    }
    // 锁外 nattch--（避免 SHM_TABLE ↔ PROCESS_TABLE 反向嵌套）。
    if !detached_shm_ids.is_empty() {
        crate::syscall::shm::shm_dec_nattch_bulk(&detached_shm_ids);
    }

    // 2) POSIX record locks are preserved across execve(2). Only locks attached
    //    to FD_CLOEXEC descriptors may have been released above through close.

    let mut table_guard = PROCESS_TABLE.lock();
    let table = table_guard.as_mut().ok_or(ExecError::NoProcess)?;

    let current_pid = table.current.ok_or(ExecError::NoProcess)?;
    let process = table.get_mut(current_pid).ok_or(ExecError::NoProcess)?;

    // execve: mmput 旧 mm（mm_users-- → 归零释放用户页）。
    //
    // 关键：此刻 satp 仍指向【旧】地址空间的页表（新 satp 要到本函数末尾才写）。
    // 旧 mm 的 Arc 一旦在 `process.mm =` 重新赋值时被 drop，`AddressSpace::drop`
    // 会把旧页表结构页（含 satp 当前指向的根页帧）释放回 frame allocator。
    // 单核下这些被释放的帧在末尾切 satp 之前不会被复用，侥幸存活；但 -smp 2 下
    // 另一 hart 的并发 frame 分配会立刻抢走并清零根页帧，导致【当前活动页表】的
    // 内核区映射凭空消失 —— 之后任何一次取指 `__alltraps` 即 InstructionPageFault
    // 无限死循环（RV execve heap-map heisenbug 真因）。
    // 故把旧 mm Arc 移出留活，待末尾写完新 satp 后再 drop，保证「活动页表的底层
    // 页帧在它仍是 satp 期间绝不被释放」。
    let old_mm_keepalive = process.mm.take();
    if let Some(ref old_mm) = old_mm_keepalive {
        old_mm.mmput();
    }

    let page_table_root = PhysAddr(new_address_space.page_table().root_ppn().0 << 12);
    process.mm = Some(alloc::sync::Arc::new(
        crate::mm::mm_struct::MmStruct::new(new_address_space),
    ));
    process.page_table_root = page_table_root;

    process.entry_point = VirtAddr(elf_info.entry);
    process.heap_start = VirtAddr(elf_info.heap_start);
    process.heap_end = VirtAddr(elf_info.program_break);
    process.user_stack_top = VirtAddr(DEFAULT_USER_STACK_TOP);
    process.user_stack_bottom = VirtAddr(DEFAULT_USER_STACK_TOP - USER_STACK_RESERVED_PAGES * PAGE_SIZE);
    process.name = String::from(name);
    process.set_comm_from_name(name);
    process.exec_path = String::from(exec_path);
    process.did_exec = true;
    // Slice 2.2: 真填 `/proc/[pid]/cmdline` —— POSIX execve(2) 用新 argv
    // 覆盖旧 cmdline。即便 argv 长度为 0 也克隆空 Vec，避免遗留旧 binary
    // 的参数串污染新进程身份。
    process.saved_argv = alloc::sync::Arc::new(argv.to_vec());
    // POSIX execve(2) signal disposition reset (full set):
    //   - Handlers that were neither SIG_DFL nor SIG_IGN revert to SIG_DFL
    //     (SIG_IGN — `handler == 1` — is preserved per POSIX 2.8.3).
    //   - Pending signal frame state is dropped — the old user-mode handler
    //     return point no longer exists in the new address space.
    //   - sigaltstack is force-disabled (POSIX: "the alternate stack shall
    //     be disabled and the SS_DISABLE flag shall be set"); old `ss_sp`
    //     points into a freed mapping.
    //   - `clear_child_tid` is zeroed — the old TID-pointer mapping is gone;
    //     keeping it would have CLONE_CHILD_CLEARTID write into stale VM
    //     during the next thread-exit path.
    //   - `sigsuspend_restore_mask`, `current_signal_flags` follow the
    //     handler reset because both refer to the pre-exec handler frame.
    //   - `pending_siginfo` is dropped on exec since any si_value payload
    //     pointed into the *old* address space; preserving it would let a
    //     post-exec SA_SIGINFO handler dereference stale pointers.
    //   - `signal_mask` itself is preserved (POSIX: "The signal mask of
    //     the process shall be unchanged.").
    for action in process.signal_handlers.iter_mut() {
        if action.handler != 1 {
            *action = crate::sched::process::SignalAction { handler: 0, flags: 0, restorer: 0, mask: 0 };
        }
    }
    process.saved_trap_context = None;
    process.saved_signal_mask = None;
    process.signal_handler_sp = 0;
    process.sigsuspend_restore_mask = None;
    process.io_wait_trap_context.clear();
    process.current_signal_flags = 0;
    process.sigaltstack_sp = 0;
    process.sigaltstack_size = 0;
    process.sigaltstack_flags = 2; // SS_DISABLE
    process.clear_child_tid = 0;
    process.pending_siginfo.clear();
    // Slice 2.5 真实化：prctl 字段 execve 五处接齐之一（reset 路径）。Linux
    // `fs/exec.c::begin_new_exec` 显式重置的项：
    //   - dumpable → SUID_DUMP_USER (1)。SUID/SGID exec 走 `suid_dumpable`
    //     sysctl（默认 0=SUID_DUMP_DISABLE），我们当前未做 SUID 探测，
    //     按 POSIX 默认 1 即可。
    //   - keepcaps → false（PR_SET_KEEPCAPS 是 task-only flag，execve 后失效）。
    //   - pdeathsig → 0（execve 后 parent-death signal 清空）。
    //   - 其余 personality / umask / no_new_privs / child_subreaper / cap_bset /
    //     seccomp_mode / audit_sessionid / loginuid / oom_score_adj /
    //     timerslack_ns / thp_disable 跨 execve **保留**，与 Linux 行为一致。
    //   - saved_envp / saved_auxv 由调用方在 push_args/setup_auxv 后再写入。
    process.dumpable = 1;
    process.keepcaps = false;
    process.pdeathsig = 0;
    // File capabilities: per Linux security/commoncap.c cap_bprm_creds_from_disk().
    // If the executable has security.capability xattr, parse VFS_CAP_REVISION_2/3
    // and transform the process capability sets accordingly.
    apply_file_capabilities(process, exec_path);
    // membarrier(2) 注册随 mm 重建在 execve 清零（Linux membarrier_exec_mmap）。
    process.membarrier_private_expedited = false;
    process.membarrier_private_sync_core = false;
    process.membarrier_global_expedited = false;
    process.mbind_policies.clear();
    // Slice 2.5 真实化：写入 PCB.saved_envp / saved_auxv（execve 提交点）
    // —— /proc/[pid]/environ 与 /proc/[pid]/auxv 的真实数据源。
    // Linux `fs/exec.c` 把这两份信息存进 `mm_struct.env_start/env_end` 与
    // `mm->saved_auxv`，我们等价存进 PCB；fork 路径会把 saved_envp/auxv
    // 一并拷给子进程（继承），exec 路径在此点整体覆盖。
    process.saved_envp = alloc::sync::Arc::new(envp.to_vec());
    process.saved_auxv = alloc::sync::Arc::new(saved_auxv);

    {
        use crate::mm::{VmaType, VmaPermission};
        let heap_start_va = VirtAddr(elf_info.heap_start);
        let heap_init_end = VirtAddr(elf_info.heap_start + PAGE_SIZE);
        let perm = VmaPermission::new(true, true, false, true);
        if let Some(ref mm) = process.mm {
            let mut addr_space = mm.lock();
            addr_space.map_region(heap_start_va, heap_init_end, VmaType::Heap, perm);
        }
    }

    // CLONE_VFORK: wake parent now that we've exec'd
    let vfork_parent_to_wake = {
        let parent_pid = process.parent_pid;
        parent_pid.and_then(|ppid| {
            let parent = table.get_mut(ppid)?;
            if parent.vfork_waiting_for == Some(current_pid) {
                parent.vfork_waiting_for = None;
                if parent.waiting_for
                    == Some(crate::sched::process::VFORK_WAIT_SENTINEL)
                {
                    parent.waiting_for = None;
                    parent.wait_channel = None;
                }
                if parent.state == ProcessState::Sleeping {
                    set_process_state(parent, ProcessState::Runnable);
                    Some(ppid)
                } else {
                    None
                }
            } else {
                None
            }
        })
    };
    if let Some(ppid) = vfork_parent_to_wake {
        if crate::cmdline::bscrash_enabled() {
            crate::println!("[VFORK] wake-exec parent={} child={}", ppid, current_pid);
        }
        table.mark_runnable(ppid, false);
    }
    let process = table.get_mut(current_pid).ok_or(ExecError::NoProcess)?;
    trap::reset_for_exec(process, elf_info, argc, envc);

    let satp = process.satp();
    if satp != 0 {
        // exec 替换映像后先激活新页表（带 ASID），再释放旧页表结构页。
        // RV/LA 都必须执行：LA 的 PGDL 若仍指向 old_mm 根页，SMP 上旧根帧
        // 一经复用，当前 hart 的下一次 TLB refill 就会遍历已释放页表。
        crate::arch::asm_helpers::activate_user_satp(satp);
    }
    // 新 satp 已生效，旧页表不再是活动页表 —— 此刻 drop 旧 mm Arc 才安全：
    // `AddressSpace::drop` 把旧页表结构页释放回 frame allocator，任何 hart 都不会
    // 再把它当作活动页表使用。
    drop(old_mm_keepalive);

    Ok(())
}

// ============================================================================
// File capabilities — per Linux security/commoncap.c
// ============================================================================
//
// Linux stores file capabilities as the `security.capability` xattr containing
// a `struct vfs_cap_data` (revision 2) or `struct vfs_ns_cap_data` (revision 3).
//
// Layout (all little-endian):
//   u32 magic_etc — bits[0..23] = magic (VFS_CAP_REVISION), bit 1 of upper byte = effective flag
//   Then for each of 2 u32-pairs (data[0], data[1]):
//     u32 permitted
//     u32 inheritable
//   data[0] = low 32 bits, data[1] = high 32 bits
//
// VFS_CAP_REVISION_2: 20 bytes total (magic + 2 * (permitted + inheritable))
// VFS_CAP_REVISION_3: 24 bytes (adds u32 rootid)

const VFS_CAP_REVISION_2: u32 = 0x02000000;
const VFS_CAP_REVISION_3: u32 = 0x02000080; // Linux uses this for v3
const VFS_CAP_FLAGS_EFFECTIVE: u32 = 0x000001;

fn apply_file_capabilities(
    process: &mut crate::sched::process::ProcessControlBlock,
    exec_path: &str,
) {
    // Read the security.capability xattr for this executable
    let xattr_data = {
        let table = crate::syscall::xattr::get_xattr_table();
        let tbl = table.lock();
        tbl.get(exec_path)
            .and_then(|attrs| attrs.get("security.capability"))
            .cloned()
    };

    let xattr_data = match xattr_data {
        Some(data) => data,
        None => {
            // No file capabilities — apply default execve transformation.
            // Per Linux: if euid==0 (root), retain full caps; otherwise
            // new_permitted = (old_inheritable & old_permitted) | old_ambient
            // new_effective = new_permitted if euid==0, else old_ambient
            if process.euid != 0 {
                let ambient = process.cap_ambient;
                let new_p = (process.cap_inheritable & process.cap_permitted) | ambient;
                process.cap_permitted = new_p;
                process.cap_effective = ambient;
            }
            return;
        }
    };

    // Parse VFS capability structure
    if xattr_data.len() < 4 { return; }
    let magic_etc = u32::from_le_bytes(xattr_data[0..4].try_into().unwrap());
    let revision = magic_etc & 0xFF00_0000;
    let has_effective = (magic_etc & VFS_CAP_FLAGS_EFFECTIVE) != 0;

    let (file_permitted, file_inheritable) = match revision {
        VFS_CAP_REVISION_2 | VFS_CAP_REVISION_3 => {
            if xattr_data.len() < 20 { return; }
            // data[0]: low 32 bits
            let perm_lo = u32::from_le_bytes(xattr_data[4..8].try_into().unwrap());
            let inh_lo = u32::from_le_bytes(xattr_data[8..12].try_into().unwrap());
            // data[1]: high 32 bits
            let perm_hi = u32::from_le_bytes(xattr_data[12..16].try_into().unwrap());
            let inh_hi = u32::from_le_bytes(xattr_data[16..20].try_into().unwrap());
            let perm = (perm_lo as u64) | ((perm_hi as u64) << 32);
            let inh = (inh_lo as u64) | ((inh_hi as u64) << 32);
            (perm, inh)
        }
        _ => return, // Unknown revision — ignore
    };

    // Per Linux security/commoncap.c cap_bprm_creds_from_disk():
    // new_permitted = (file_inheritable & thread_inheritable) |
    //                 (file_permitted & thread_bounding_set) |
    //                 thread_ambient
    let new_permitted = (file_inheritable & process.cap_inheritable)
        | (file_permitted & process.cap_bset)
        | process.cap_ambient;
    process.cap_permitted = new_permitted;

    // new_effective:
    //   if file has fE bit set → new_effective = new_permitted
    //   else → new_effective = ambient only
    if has_effective {
        process.cap_effective = new_permitted;
    } else {
        process.cap_effective = process.cap_ambient;
    }

    // Per Linux: cap_ambient is cleared after the transformation
    process.cap_ambient = 0;
}
