//! Process creation: fork/clone, init bootstrap, ELF/path-based creation.
//!
//! Extracted from `process.rs` to align with Linux `kernel/fork.c`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use crate::mm::{PhysAddr, VirtAddr, AddressSpace};
use super::process::{
    ProcessControlBlock, ProcessTable, PROCESS_TABLE,
    Pid, NEXT_PID, maybe_print_heap_snapshot,
    install_user_stack, DEFAULT_USER_STACK_TOP,
    USER_STACK_RESERVED_PAGES, USER_STACK_PREFAULT_PAGES,
};

/// Fork - 创建子进程
///
/// 复制当前进程，创建一个几乎完全相同的子进程。
/// 父进程返回子进程 PID，子进程返回 0。
///
/// # 地址空间复制
/// 使用写时复制（CoW）机制复制父进程的地址空间，
/// 子进程获得独立的页表，但物理页在写入时才复制。
pub fn fork() -> Result<(Pid, usize, usize), &'static str> {
    fork_with_ctx(None, 0)
}

/// fork 的内部实现，可选地在持有锁的情况下直接初始化 TrapContext
/// clone_flags: raw clone flags (CLONE_VM etc.) to influence address space handling
pub fn fork_with_ctx(parent_ctx: Option<&crate::arch::TrapContext>, clone_flags: usize) -> Result<(Pid, usize, usize), &'static str> {
    static FORK_SEQ: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let seq = FORK_SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // ===== Phase A：PT 下解析 + PCB/字段继承 + 快照（对标 Linux copy_process
    // 前半：tasklist 锁只保护表状态；dup_mm / 内核栈分配在 Phase B 锁外）。
    // 实测 8 核编译期 fork 大地址空间进程（tokio shim/rustc）clone_cow 在 PT
    // 临界区内跑出单次持锁 1.06s（pt_lock max_hold），全体 hart 排队。=====
    let (child_pid, parent_pid, parent_cgroup, mut child, parent_mm_arc, parent_pt_root, inherited_shm_ids) = {
        let mut table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_mut().ok_or("Process table not initialized")?;

        // 获取当前进程
        let parent_pid = table.current.ok_or("No current process")?;
        maybe_print_heap_snapshot(table, "fork");

    // cgroup pids.max enforcement: check if the parent's cgroup allows fork
    {
        let parent_cgroup = table.get(parent_pid)
            .map(|p| p.cgroup_path.clone())
            .unwrap_or_else(|| String::from("/"));
        if !crate::kernel::cgroup::cgroup_pids_can_fork(&parent_cgroup) {
            return Err("EAGAIN: cgroup pids.max exceeded");
        }
    }

    // 分配新 PID
    let child_pid = table.alloc_pid();

    // 创建子进程控制块
    // new_for_fork：不安装默认 stdio fd 表（下方直接用父表覆盖）
    let mut child = ProcessControlBlock::new_boxed_for_fork(child_pid);

    // Snapshot the caller-visible descriptor table before taking a mutable
    // borrow of the parent.  A CLONE_FILES sharer/thread may redirect fd
    // operations to another PCB via `fd_table_owner`, so copying `parent.fd_table`
    // directly would incorrectly fork an empty/stale local table.
    let (parent_fd_table, parent_fd_next_hint, parent_cloexec_bits, parent_cloexec_fds) = {
        let fd_owner = table
            .get_fd_process(parent_pid)
            .ok_or("Parent fd table not found")?;
        (
            fd_owner.fd_table.clone(),
            fd_owner.fd_next_hint,
            fd_owner.cloexec_bits,
            fd_owner.cloexec_fds.clone(),
        )
    };

    // 先获取父进程的可变引用，复制 PCB 字段；**本借用块的尾表达式产出 Phase B
    // 所需快照（Arc/值拷贝）**——块尾结束 parent 借用（NLL），值经 let 提升到
    // Phase A 外层作用域，Phase A 块尾再统一返回（锁外 Phase B 使用）。
    let (parent_mm_arc, parent_pt_root, parent_cgroup, inherited_shm_ids) = {
        let parent = table.get_mut(parent_pid).ok_or("Parent process not found")?;

        child.parent_pid = Some(parent_pid);
        child.name = parent.name.clone();
        child.comm = parent.comm.clone();
        child.exec_path = parent.exec_path.clone();
        // The child inherits exec_path (for /proc/self/exe) but NOT the
        // exec-deny ownership: fork does not increment the write-deny count,
        // so only the process that actually execve'd releases it. A fork'd
        // child therefore starts with an empty exec_deny_path and only
        // populates it on its own execve.
        child.exec_deny_path = String::new();
        // Slice 2.2: POSIX `fork(2)` 继承父的 cmdline (`/proc/[pid]/cmdline`
        // 直到 child 自己 execve 才覆写)。复制 Vec<String> 整体语义安全
        // (没有共享内部缓冲)。
        child.saved_argv = parent.saved_argv.clone();
        child.uid = parent.uid;
        child.euid = parent.euid;
        child.suid = parent.suid;
        child.gid = parent.gid;
        child.egid = parent.egid;
        child.sgid = parent.sgid;
        child.supplementary_groups = parent.supplementary_groups.clone();
        child.dumpable = parent.dumpable;
        child.no_new_privs = parent.no_new_privs;
        child.keepcaps = parent.keepcaps;
        child.thp_disable = parent.thp_disable;
        child.child_subreaper = false;
        // membarrier 注册跨 fork 继承（Linux membarrier_fork 复制 mm 的状态）。
        child.membarrier_private_expedited = parent.membarrier_private_expedited;
        child.membarrier_private_sync_core = parent.membarrier_private_sync_core;
        child.membarrier_global_expedited = parent.membarrier_global_expedited;
        child.timerslack_ns = parent.timerslack_ns;
        child.default_timerslack_ns = parent.timerslack_ns;
        child.pdeathsig = 0;
        child.cap_bset = parent.cap_bset;
        // Slice 2.x 真化〔去 capget/capset silent 0〕：cap 三集 fork 继承父，
        // 与 Linux `copy_creds()` 一致——effective/permitted/inheritable 全 copy。
        child.cap_effective = parent.cap_effective;
        child.cap_permitted = parent.cap_permitted;
        child.cap_inheritable = parent.cap_inheritable;
        child.cap_ambient = parent.cap_ambient;
        // Slice 2.5 真实化：fork/clone 继承 Linux task_struct 中以下字段
        // （`copy_process()` 路径，参考 kernel/fork.c）。securebits/seccomp_mode
        // 走 cred 子系统但 fork 不重置；audit_sessionid 与 loginuid 同样
        // PAM 设定后不随 fork/setuid 改变；oom_score_adj 复制；auxv/envp/mm_*
        // 由 execve 重写（fork 后 child 共享 image，需要继承）。
        child.securebits = parent.securebits;
        child.seccomp_mode = parent.seccomp_mode;
        // 真化〔BPF filter fork 继承〕：Arc::clone 而非 deep clone（与 Linux
        // `__seccomp_filter` 一致，filter 节点共享，引用计数到位即可）。
        child.seccomp_filters = parent.seccomp_filters.clone();
        child.audit_sessionid = parent.audit_sessionid;
        child.oom_score_adj = parent.oom_score_adj;
        child.saved_auxv = parent.saved_auxv.clone();
        child.saved_envp = parent.saved_envp.clone();
        // last_syscall 是当前正在执行的 syscall —— fork 出来的 child 在 trap
        // 返回时也"刚执行完 fork"，所以从 dispatcher 出口 clear 即可；这里
        // 保持 child PCB::new 的初值（u64::MAX = running）。
        child.mm_start_code = parent.mm_start_code;
        child.mm_end_code = parent.mm_end_code;
        child.mm_start_data = parent.mm_start_data;
        child.mm_end_data = parent.mm_end_data;
        child.mm_start_brk = parent.mm_start_brk;
        child.mm_brk = parent.mm_brk;
        child.mm_start_stack = parent.mm_start_stack;
        child.mm_arg_start = parent.mm_arg_start;
        child.mm_arg_end = parent.mm_arg_end;
        child.mm_env_start = parent.mm_env_start;
        child.mm_env_end = parent.mm_env_end;
        child.pidns_root = parent.pidns_root;
        child.clone_ns_flags = parent.clone_ns_flags;
        // 命名空间继承/隔离 — 对标 Linux copy_namespaces()
        {
            use crate::kernel::namespace::*;
            let ns_flags = clone_flags as i32;
            let has_any_ns = ns_flags & (CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWPID
                | CLONE_NEWNS | CLONE_NEWNET | CLONE_NEWUSER | CLONE_NEWCGROUP | CLONE_NEWTIME) != 0;
            if has_any_ns {
                // 至少有一个 CLONE_NEW* flag → 基于父 NsProxy 做按需 clone
                let parent_ns = parent.nsproxy.lock();
                let new_ns = NsProxy {
                    uts_ns: if ns_flags & CLONE_NEWUTS != 0 {
                        parent_ns.uts_ns.clone_ns()
                    } else {
                        UtsNamespace { id: parent_ns.uts_ns.id, hostname: parent_ns.uts_ns.hostname.clone(), domainname: parent_ns.uts_ns.domainname.clone() }
                    },
                    user_ns: if ns_flags & CLONE_NEWUSER != 0 {
                        parent_ns.user_ns.clone_ns()
                    } else {
                        UserNamespace { id: parent_ns.user_ns.id, uid_map: parent_ns.user_ns.uid_map.clone(), gid_map: parent_ns.user_ns.gid_map.clone(), setgroups_allowed: parent_ns.user_ns.setgroups_allowed, owner_uid: parent_ns.user_ns.owner_uid }
                    },
                    pid_ns: if ns_flags & CLONE_NEWPID != 0 {
                        parent_ns.pid_ns.clone_ns()
                    } else {
                        PidNamespace { id: parent_ns.pid_ns.id, level: parent_ns.pid_ns.level, pid_map: parent_ns.pid_ns.pid_map.clone(), next_pid: core::sync::atomic::AtomicUsize::new(parent_ns.pid_ns.next_pid.load(core::sync::atomic::Ordering::Relaxed)) }
                    },
                    mnt_ns: if ns_flags & CLONE_NEWNS != 0 {
                        parent_ns.mnt_ns.clone_ns()
                    } else {
                        MountNamespace { id: parent_ns.mnt_ns.id, mount_points: parent_ns.mnt_ns.mount_points.clone() }
                    },
                    net_ns: if ns_flags & CLONE_NEWNET != 0 {
                        parent_ns.net_ns.clone_ns()
                    } else {
                        NetNamespace { id: parent_ns.net_ns.id, loopback_up: parent_ns.net_ns.loopback_up }
                    },
                    ipc_ns: if ns_flags & CLONE_NEWIPC != 0 {
                        parent_ns.ipc_ns.clone_ns()
                    } else {
                        IpcNamespace { id: parent_ns.ipc_ns.id }
                    },
                    cgroup_ns: if ns_flags & CLONE_NEWCGROUP != 0 {
                        parent_ns.cgroup_ns.clone_ns()
                    } else {
                        CgroupNamespace { id: parent_ns.cgroup_ns.id, root_cgroup: parent_ns.cgroup_ns.root_cgroup.clone() }
                    },
                    // timens_on_fork: the child joins its parent's
                    // time_ns_for_children (set up by unshare(CLONE_NEWTIME) +
                    // /proc/self/timens_offsets), inheriting the offsets.
                    time_ns: TimeNamespace {
                        id: parent_ns.time_ns_for_children.id,
                        monotonic_offset_ns: parent_ns.time_ns_for_children.monotonic_offset_ns,
                        boottime_offset_ns: parent_ns.time_ns_for_children.boottime_offset_ns,
                    },
                    time_ns_for_children: TimeNamespace {
                        id: parent_ns.time_ns_for_children.id,
                        monotonic_offset_ns: parent_ns.time_ns_for_children.monotonic_offset_ns,
                        boottime_offset_ns: parent_ns.time_ns_for_children.boottime_offset_ns,
                    },
                };
                // clone(CLONE_NEWNS) materialises a brand-new mount namespace
                // for the child: snapshot the parent's current mount tree into
                // it (对标 copy_mnt_ns) so the child starts with an independent
                // but identical view. Peer groups are preserved so later shared
                // mounts propagate across the boundary.
                let mnt_ns_cloned = ns_flags & CLONE_NEWNS != 0;
                let parent_mnt_id = parent_ns.mnt_ns.id.0;
                let child_mnt_id = new_ns.mnt_ns.id.0;
                drop(parent_ns);
                if mnt_ns_cloned {
                    crate::fs::mount::snapshot_mount_info(parent_mnt_id, child_mnt_id);
                    crate::fs::snapshot_mount_ns(parent_mnt_id, child_mnt_id);
                }
                child.nsproxy = alloc::sync::Arc::new(crate::sync::SpinLock::new(new_ns));
                // Expose the child's namespace ids under /proc/<pid>/ns/* so
                // that another process can setns() into them.
                {
                    let g = child.nsproxy.lock();
                    crate::kernel::namespace::register_process_ns(child_pid, &g);
                }
            } else {
                // 无 CLONE_NEW* → 默认共享父 NsProxy（same Arc）。
                // 例外：若父进程此前 unshare(CLONE_NEWTIME) 并写了
                // timens_offsets（time_ns_for_children 与 time_ns 偏移不同），
                // 则按 timens_on_fork：子进程必须进入父的 time_ns_for_children，
                // 故给子进程一份私有 NsProxy（其余命名空间身份不变）。
                let parent_ns = parent.nsproxy.lock();
                let timens_differs =
                    parent_ns.time_ns_for_children.monotonic_offset_ns != parent_ns.time_ns.monotonic_offset_ns
                    || parent_ns.time_ns_for_children.boottime_offset_ns != parent_ns.time_ns.boottime_offset_ns;
                if timens_differs {
                    let mut child_ns = parent_ns.copy_with_mnt_ns(parent_ns.mnt_ns.id.0);
                    child_ns.time_ns = TimeNamespace {
                        id: parent_ns.time_ns_for_children.id,
                        monotonic_offset_ns: parent_ns.time_ns_for_children.monotonic_offset_ns,
                        boottime_offset_ns: parent_ns.time_ns_for_children.boottime_offset_ns,
                    };
                    drop(parent_ns);
                    child.nsproxy = alloc::sync::Arc::new(crate::sync::SpinLock::new(child_ns));
                    let g = child.nsproxy.lock();
                    crate::kernel::namespace::register_process_ns(child_pid, &g);
                } else {
                    drop(parent_ns);
                    child.nsproxy = parent.nsproxy.clone();
                }
            }
        }
        child.cached_mnt_ns_id = child.nsproxy.lock().mnt_ns.id.0;
        // Linux: exit_signal is the low byte of clone flags (clone2 ABI).
        // clone3 merges exit_signal into flags before calling sys_clone.
        let sig = (clone_flags & 0xff) as i32;
        child.exit_signal = if sig == 0 { 17 } else { sig }; // default SIGCHLD
        // kcmp(2) resource-share ids: when the matching CLONE_* flag shares the
        // resource the child inherits the parent's id, otherwise it keeps its
        // own (set in PCB::new). Lets kcmp() report FS/IO/SYSVSEM as shared
        // across distinct tgids (kcmp03).
        {
            const CLONE_FS_FLAG: usize = 0x0000_0200;
            const CLONE_SYSVSEM_FLAG: usize = 0x0004_0000;
            const CLONE_IO_FLAG: usize = 0x8000_0000;
            if clone_flags & CLONE_FS_FLAG != 0 { child.fs_share_id = parent.fs_share_id; }
            // Linux copy_fs：CLONE_FS 共享同一 fs_struct（chdir/chroot/umask
            // 全组可见），否则深拷贝私有副本。
            child.fs = if clone_flags & CLONE_FS_FLAG != 0 {
                parent.fs.clone()
            } else {
                alloc::sync::Arc::new(crate::sync::SpinLock::new(parent.fs.lock().clone()))
            };
            if clone_flags & CLONE_SYSVSEM_FLAG != 0 { child.sysvsem_share_id = parent.sysvsem_share_id; }
            if clone_flags & CLONE_IO_FLAG != 0 { child.io_share_id = parent.io_share_id; }
        }
        // Slice 2.2: audit `loginuid` 必须按 Linux `kernel/auditsc.c` 继承
        // 父值（PAM 在 login 时 写入 ancestor 的 loginuid，fork/setuid 不
        // 重置；只有 `/proc/[pid]/loginuid` 显式 write 才会改）。其余 io_*
        // / voluntary_ctxt_switches / start_time_ticks 保持 child 自身值
        // （PCB::new 已用 child 创建时刻初始化），与 Linux `copy_process`
        // 路径一致 —— fork 后 child 的资源累计计数从 0 开始。
        child.loginuid = parent.loginuid;
        // Landlock domain: inherited on fork (per Linux security/landlock/cred.c)
        child.landlock_domain = parent.landlock_domain.clone();
        // cgroup: child inherits parent's cgroup membership (Linux copy_process)
        child.cgroup_path = parent.cgroup_path.clone();
        child.entry_point = parent.entry_point;
        child.user_stack_top = parent.user_stack_top;
        child.user_stack_bottom = parent.user_stack_bottom;
        child.heap_start = parent.heap_start;
        child.heap_end = parent.heap_end;

        // 复制调用者可见的文件描述符表（new_for_fork 未安装 stdio 表，无需先 discard）
        child.fd_table = parent_fd_table;
        for fd_obj in child.fd_table.iter().flatten() {
            fd_obj.inc_fd_ref();
        }
        child.fd_table_owner = child_pid;
        child.fd_next_hint = parent_fd_next_hint.min(child.fd_table.len());
        child.cloexec_bits = parent_cloexec_bits;
        child.cloexec_fds = parent_cloexec_fds;
        child.rlimit_nofile = parent.rlimit_nofile;
        child.rlimit_nofile_max = parent.rlimit_nofile_max;
        // 复制 16 项 RLIMIT 数组；NOFILE 已经通过 rlimit_nofile/_max 同步，
        // 但其它 14 项（CPU/FSIZE/DATA/STACK/...）只在 rlimits 里，必须 copy。
        child.rlimits = parent.rlimits;
        child.cputime_precise = parent.cputime_precise;
        child.signal_handlers = parent.signal_handlers;
        child.signal_mask = parent.signal_mask;
        child.pgid = parent.pgid;
        child.sid = parent.sid;
        // Linux: SCHED_DEADLINE is NOT inherited by fork; child gets SCHED_NORMAL.
        // SCHED_RESET_ON_FORK: 子进程强制回 SCHED_NORMAL/nice≥0，并清除标志。
        if parent.sched_reset_on_fork {
            child.sched_policy = 0;
            child.sched_priority = 0;
            child.sched_reset_on_fork = false;
            child.dl_runtime = 0;
            child.dl_deadline = 0;
            child.dl_period = 0;
        } else if parent.sched_policy == 6 { // SCHED_DEADLINE
            child.sched_policy = 0; // SCHED_NORMAL
            child.sched_priority = 0;
            child.dl_runtime = 0;
            child.dl_deadline = 0;
            child.dl_period = 0;
        } else {
            child.sched_policy = parent.sched_policy;
            child.sched_priority = parent.sched_priority;
            // SCHED_RR: child gets a fresh time slice
            child.dl_runtime = 0;
            child.dl_deadline = 0;
            child.dl_period = 0;
        }
        child.nice = if parent.sched_reset_on_fork && parent.nice < 0 {
            0 // Linux: reset_on_fork 下负 nice 不继承
        } else {
            parent.nice
        };
        child.sched_weight = if child.nice != parent.nice {
            super::process::nice_to_weight(child.nice)
        } else {
            parent.sched_weight
        };
        // 父任务可能正在运行：fork 继承前先取 update_curr 后的有效值。
        let now_ns = crate::sched::policy::now_ns();
        let (parent_vrt, parent_dl) = super::process::effective_cfs_state(parent, now_ns);
        child.vruntime = parent_vrt;
        child.deadline = parent_dl;
        child.slice_ns = parent.slice_ns;
        child.vlag = 0;
        child.cpus_allowed = parent.cpus_allowed;
        child.rq_cpu = None;
        child.wake_cpu = None;
        child.personality = parent.personality;
        child.mempolicy_mode = parent.mempolicy_mode;
        child.mempolicy_nodemask = parent.mempolicy_nodemask;
        child.mbind_policies = parent.mbind_policies.clone();
        // 复制 SHM attachments（字段继承；重映射在 Phase B 按父页表进行）
        child.shm_attachments = parent.shm_attachments.clone();
        // 锁外 Phase B 所需快照（Arc/值拷贝，出锁后安全）。
        (
            parent.mm.clone(),
            parent.page_table_root,
            parent.cgroup_path.clone(),
            child.shm_attachments.iter().map(|&(id, _)| id).collect::<Vec<i32>>(),
        )
    };
        (child_pid, parent_pid, parent_cgroup, child, parent_mm_arc, parent_pt_root, inherited_shm_ids)
    };

    // ===== Phase B：锁外 dup_mm + 栈/上下文初始化（对标 Linux dup_mm：copy_mm
    // 不持 tasklist 锁，页表/物理页工作不阻塞其它 hart 的进程表操作）=====
    let mut needs_parent_tlb_drain = false;
    const CLONE_VM_FLAG: usize = 0x0000_0100;
    if crate::cmdline::bscrash_enabled() {
        crate::println!(
            "[FORK] copy_mm parent={} child={} clone_flags={:#x} vm_share={}",
            parent_pid, child_pid, clone_flags,
            clone_flags & CLONE_VM_FLAG != 0
        );
    }
    if clone_flags & CLONE_VM_FLAG != 0 {
        // CLONE_VM: 共享父进程地址空间（vfork/线程语义）。
        // Arc::clone + mmget() 替代旧的 shared_vm_parent hack。
        if let Some(ref parent_mm) = parent_mm_arc {
            parent_mm.mmget();
            child.mm = Some(parent_mm.clone());
        }
        child.page_table_root = parent_pt_root;
    } else if let Some(ref parent_mm) = parent_mm_arc {
        let child_addr_space = {
            let mut parent_as = parent_mm.lock();
            let r = parent_as
                .clone_cow()
                .ok_or("ENOMEM: out of memory cloning address space for fork")?;
            r
        };
        // clone_cow 已把父 PTE 改只读并发异步 shootdown；必须在 child 可运行前
        // 做强 drain，保证其它父线程不再通过旧 writable TLB 修改子进程的 CoW
        // 快照（drain 在本函数尾部 Phase C 之后执行）。
        needs_parent_tlb_drain = true;
        child.page_table_root = PhysAddr(child_addr_space.page_table().root_ppn().0 << 12);
        child.mm = Some(alloc::sync::Arc::new(
            crate::mm::mm_struct::MmStruct::new(child_addr_space),
        ));
    } else {
        child.create_address_space()?;
    }

    // 在子进程地址空间中重映射 SHM 物理页（CoW 不处理 SHM）
    // 直接从父进程页表读取物理页映射（避免依赖 SHM_TABLE，因为 IPC_RMID 可能已删除段）
    {
        use crate::mm::{VirtPageNum as VPN, PTEFlags};
        let shm_list = child.shm_attachments.clone();
        if !shm_list.is_empty() && parent_mm_arc.is_some() {
            // 从父进程页表读取 SHM 区域的物理映射
            let parent_as = parent_mm_arc.as_ref().unwrap().lock();
            let mut shm_mappings: Vec<(VPN, crate::mm::PhysPageNum, PTEFlags)> = Vec::new();
            for &(shmid, base) in &shm_list {
                // 只扫描本段真实页数（旧实现固定 256 页：段小于 256 页或与相邻
                // VMA 接壤时，会把父页表里邻接区域也抄进 child，产生无 VMA 覆盖
                // 的用户 PTE——FRAMEDBG 实锤 [PTE-LEAK] va=0x41122000 site=None）。
                let seg_pages = crate::ipc::sysv_shm::SHM_TABLE
                    .lock()
                    .as_ref()
                    .and_then(|t| t.get(&shmid))
                    .map(|s| (s.size + 4095) / 4096)
                    .unwrap_or(0);
                if seg_pages == 0 {
                    continue;
                }
                for i in 0..seg_pages.min(256) {
                    let vpn = VPN((base + i * 4096) / 4096);
                    if let Some(ppn) = parent_as.page_table().translate(vpn) {
                        // Preserve the parent's protection (notably the missing
                        // W bit for a SHM_RDONLY attach) so the child inherits a
                        // read-only attach and a write still faults (shmat01).
                        let flags = parent_as
                            .page_table()
                            .get_pte_flags(vpn)
                            .unwrap_or(PTEFlags::R | PTEFlags::W | PTEFlags::U);
                        shm_mappings.push((vpn, ppn, flags));
                    } else {
                        break;
                    }
                }
            }
            drop(parent_as);
            if let Some(ref child_mm) = child.mm {
                let mut addr_space = child_mm.lock();
                for &(vpn, ppn, flags) in &shm_mappings {
                    // 二道保险：child 必须有覆盖该页的 VMA 才建 PTE，宁缺勿滥
                    // （缺页重走 shmat 语义，也不会在进程退出时留下 VMA 外 PTE）。
                    let va = vpn.0 * 4096;
                    if !addr_space.range_fully_mapped(
                        crate::mm::VirtAddr(va),
                        crate::mm::VirtAddr(va + 4096),
                    ) {
                        continue;
                    }
                    if addr_space.page_table().translate(vpn).is_some() {
                        addr_space.page_table_mut().unmap(vpn);
                    }
                    addr_space.page_table_mut().map(vpn, ppn, flags);
                }
            }
        }
    }

    child.allocate_kernel_stack()?;

    // 如果提供了父进程的 TrapContext，在持有锁的情况下直接复制并初始化
    // 使用 black_box 防止编译器优化导致的寄存器分配错误
    if let Some(src_ctx) = parent_ctx {
            let tcp = core::hint::black_box(child.trap_context_ptr);
            let ksp = child.kernel_stack_top();
            if tcp != 0 {
                unsafe {
                    // 使用内联汇编复制，完全绕过编译器
                    #[cfg(target_arch = "loongarch64")]
                    {
                        let dst_base: usize = tcp;
                        let src_base: usize = src_ctx as *const crate::arch::TrapContext as usize;
                        let total_bytes: usize = core::mem::size_of::<crate::arch::TrapContext>();
                        core::arch::asm!(
                            "   move  $t2, $zero",
                            "2: beq   $t2, {len}, 3f",
                            "   ldx.d $t3, {src}, $t2",
                            "   stx.d $t3, {dst}, $t2",
                            "   addi.d $t2, $t2, 8",
                            "   b     2b",
                            "3:",
                            dst = in(reg) dst_base,
                            src = in(reg) src_base,
                            len = in(reg) total_bytes,
                            out("$t2") _,
                            out("$t3") _,
                        );
                        // 设置子进程返回值 = 0 (a0 = x[4])
                        let child_ctx = &mut *(tcp as *mut crate::arch::TrapContext);
                        core::ptr::write_volatile(&mut child_ctx.x[4] as *mut usize, 0);
                        core::ptr::write_volatile(&mut child_ctx.kernel_sp as *mut usize, ksp);
                    }
                    #[cfg(not(target_arch = "loongarch64"))]
                    {
                        let dst = tcp as *mut u8;
                        let src = src_ctx as *const crate::arch::TrapContext as *const u8;
                        let size = core::mem::size_of::<crate::arch::TrapContext>();
                        let dst_u64 = dst as *mut u64;
                        let src_u64 = src as *const u64;
                        let count = size / 8;
                        for i in 0..count {
                            core::ptr::write_volatile(dst_u64.add(i), core::ptr::read(src_u64.add(i)));
                        }
                        let child_ctx = &mut *(tcp as *mut crate::arch::TrapContext);
                        child_ctx.x[10] = 0; // RISC-V: a0 = x[10]
                        child_ctx.kernel_sp = ksp;
                    }
                }
            }
        }

    // ===== Phase C：PT 下插入（对标 Linux 插入 tasklist + wake_up_new_task
    // 之前的表操作；子进程 trap 上下文此刻已就绪，插入后即可被调度）=====
    // 不设置 Runnable — 由 sys_clone 在初始化 TrapContext 后设置
    // 避免跨 tgid 抢占在 TrapContext 初始化前调度子进程
    // 在插入前保存关键值（避免后续从 BTreeMap 重新查找时的编译器优化问题）
    let child_tcp = child.trap_context_ptr;
    let child_ksp = child.kernel_stack_top();
    {
        let mut table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_mut().ok_or("Process table not initialized")?;

        // 将子进程添加到父进程的子进程列表
        if let Some(parent) = table.get_mut(parent_pid) {
            parent.children.push(child_pid);
        }

        // cgroup v1 memory：子进程继承父进程所属 memcg（对标 v1 fork 自动加入）。
        // on_fork 仅短暂锁定 mm::memcg 内部表、不回锁 PROCESS_TABLE，嵌套安全。
        crate::mm::memcg::on_fork(parent_pid, child_pid);

        // cgroup pids.current++: charge after fork succeeds（cgroup 路径在
        // Phase A 已快照）
        crate::kernel::cgroup::cgroup_pids_charge(&parent_cgroup);

        // 插入进程表
        table.insert(child_pid, child);
        // PROCESS_TABLE 仍在锁中，但 SHM 内部锁与 PROCESS_TABLE 之间是单向
        // 嵌套（shm.rs/ipc::sysv 从不在持 SHM_* 锁时回锁 PROCESS_TABLE 来调
        // 用 nattch helper），所以此处嵌套安全。
        if !inherited_shm_ids.is_empty() {
            crate::syscall::shm::shm_inc_nattch_bulk(&inherited_shm_ids);
        }
    }
    // crate::println!("[FORK] child_pid={} done", child_pid);
    if needs_parent_tlb_drain {
        crate::mm::tlb::drain_shootdown();
    }
    Ok((child_pid, child_tcp, child_ksp))
}

pub fn create_init_process() -> Result<Pid, &'static str> {
    let mut table = PROCESS_TABLE.lock();
    let table = table.as_mut().ok_or("Process table not initialized")?;

    let mut init = Box::new(ProcessControlBlock::new(1));
    init.name = String::from("init");
    init.set_comm_from_name("init");
    init.exec_path = String::from("init");
    init.saved_argv = alloc::sync::Arc::new(alloc::vec![String::from("init")]);
    init.parent_pid = None;

    // 为 init 进程创建地址空间
    init.create_address_space()?;

    table.insert_runnable(1, init);
    if !table.switch_to(1, 0) {
        return Err("init process was already claimed");
    }

    // 重置 PID 计数器确保从 2 开始
    NEXT_PID.store(2, Ordering::SeqCst);

    Ok(1)
}

pub fn create_process_from_elf(name: &str, elf_data: &[u8]) -> Result<Pid, &'static str> {
    use crate::loader::ElfLoader;

    let mut table = PROCESS_TABLE.lock();
    let table = table.as_mut().ok_or("Process table not initialized")?;

    // 分配新 PID
    let pid = table.alloc_pid();

    // 创建进程控制块
    let mut process = Box::new(ProcessControlBlock::new(pid));
    process.name = String::from(name);
    process.set_comm_from_name(name);
    process.exec_path = String::from(name);
    process.saved_argv = alloc::sync::Arc::new(alloc::vec![String::from(name)]);

    // 创建地址空间
    let mut address_space = AddressSpace::new();

    // 加载 ELF 文件并分配用户栈
    {
        let elf_info = ElfLoader::load(elf_data, &mut address_space)
            .map_err(|_| "Failed to load ELF")?;

        crate::println!("[elf] entry={:#x} base={:#x} end={:#x} heap={:#x}",
            elf_info.entry, elf_info.base, elf_info.end, elf_info.heap_start);

        // 设置进程信息
        process.entry_point = VirtAddr(elf_info.entry);
        process.heap_start = VirtAddr(elf_info.heap_start);
        process.heap_end = VirtAddr(elf_info.program_break);

        // 更新页表根地址
        let (stack_bottom, stack_top) = install_user_stack(
            &mut address_space,
            DEFAULT_USER_STACK_TOP,
            USER_STACK_RESERVED_PAGES,
            USER_STACK_PREFAULT_PAGES,
        )?;

        process.page_table_root = PhysAddr(address_space.page_table().root_ppn().0 << 12);
        process.user_stack_top = stack_top;
        process.user_stack_bottom = stack_bottom;
    }

    process.mm = Some(alloc::sync::Arc::new(
        crate::mm::mm_struct::MmStruct::new(address_space),
    ));

    // 分配内核栈
    process.allocate_kernel_stack()?;

    // 初始化 TrapContext
    process.init_trap_context()?;

    // 验证 TrapContext 设置
    unsafe {
        if let Some(ctx) = process.trap_context() {
            #[cfg(target_arch = "riscv64")]
            crate::println!("[init] TrapContext: sepc={:#x} sp={:#x} ksp={:#x} satp={:#x}",
                ctx.sepc, ctx.x[2], ctx.kernel_sp, ctx.kernel_satp);
            #[cfg(target_arch = "loongarch64")]
            crate::println!("[init] TrapContext: sepc={:#x} sp={:#x} ksp={:#x} prmd={:#x}",
                ctx.sepc, ctx.x[3], ctx.kernel_sp, ctx.sstatus);
        }
    }

    // 插入进程表
    table.insert_runnable(pid, process);

    Ok(pid)
}

pub fn create_process_from_path(path: &str) -> Result<Pid, &'static str> {
    use crate::loader::ElfLoader;

    let mut table = PROCESS_TABLE.lock();
    let table = table.as_mut().ok_or("Process table not initialized")?;

    // 1. 分配新 PID
    let pid = table.alloc_pid();

    // 2. 创建进程控制块
    let mut process = Box::new(ProcessControlBlock::new(pid));
    process.name = String::from(path);
    process.set_comm_from_name(path);
    process.exec_path = String::from(path);
    process.saved_argv = alloc::sync::Arc::new(alloc::vec![String::from(path)]);

    // 3. 创建地址空间（包含独立的 Sv39 页表）
    // Requirements 4.1: 创建新的 Sv39 页表
    let mut address_space = AddressSpace::new();

    // 4. 从文件系统加载 ELF
    // Requirements 3.4: 通过 VFS 接口读取 ELF 数据
    let elf_info = ElfLoader::load_from_fs(path, &mut address_space)
        .map_err(|e| {
            crate::println!("[process] Failed to load ELF from {}: {}", path, e);
            "Failed to load ELF from filesystem"
        })?;

    // 5. 设置进程信息
    process.entry_point = VirtAddr(elf_info.entry);
    process.heap_start = VirtAddr(elf_info.heap_start);
    process.heap_end = VirtAddr(elf_info.program_break);

    // 6. 分配用户栈
    let (stack_bottom, stack_top) = install_user_stack(
        &mut address_space,
        DEFAULT_USER_STACK_TOP,
        USER_STACK_RESERVED_PAGES,
        USER_STACK_PREFAULT_PAGES,
    )?;
    process.user_stack_top = stack_top;
    process.user_stack_bottom = stack_bottom;

    // 更新页表根地址
    process.page_table_root = PhysAddr(address_space.page_table().root_ppn().0 << 12);

    // 包装为 Arc<MmStruct>
    process.mm = Some(alloc::sync::Arc::new(
        crate::mm::mm_struct::MmStruct::new(address_space),
    ));

    // 7. 分配内核栈
    process.allocate_kernel_stack()?;

    // 8. 初始化 TrapContext
    // Requirements 5.1, 5.2, 5.3, 5.4
    process.init_trap_context()?;

    // 10. 加入进程表
    table.insert_runnable(pid, process);

    Ok(pid)
}
