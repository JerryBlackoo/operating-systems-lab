//! ProcFS 内核状态监控文件系统
//!
//! 实现类似 Linux 的 /proc 文件系统，导出内核状态。
//!
//! # 特性
//! - 进程信息 (/proc/[pid]/status)
//! - 内存信息 (/proc/meminfo)
//! - CPU 信息 (/proc/cpuinfo)
//! - 调度器统计 (/proc/sched_stat)
//! - 能力树 (/proc/capabilities)

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::format;
use crate::sync::SpinLock;
use crate::fs::proc::ipc::{
    SysvipcMsgGenerator, SysvipcSemGenerator, SysvipcShmGenerator,
};
use crate::fs::proc::pid::{
    get_all_pids, get_current_pid, open_fd_names, pid_tgid, FdInfoGenerator, KpageflagsGenerator,
};
use crate::fs::proc::system::{
    CapabilitiesGenerator, CpuinfoGenerator, IoUringGenerator, LoadavgGenerator, MeminfoGenerator,
    FilesystemsGenerator, InterruptsGenerator, KernelCmdlineGenerator, KernelConfigGzGenerator,
    MountsGenerator, SchedStatGenerator, StatGenerator, UptimeGenerator, VersionGenerator,
};


/// ProcFS 错误
#[derive(Debug, Clone, Copy)]
pub enum ProcError {
    /// 文件不存在
    NotFound,
    /// 只读文件
    ReadOnly,
    /// 无效参数
    InvalidArgument,
    /// 权限不足
    PermissionDenied,
    /// 操作不允许 (EPERM)
    OperationNotPermitted,
    /// 无效写入数据
    InvalidData,
    /// 结果超出范围 (ERANGE)
    OutOfRange,
}

/// 文件内容生成器 trait
pub trait ProcFileGenerator: Send + Sync {
    /// 生成文件内容
    fn generate(&self) -> String;

    /// Slice 2.5：生成二进制内容。Linux `/proc/[pid]/auxv` / `environ` 等
    /// 文件含非 UTF-8 字节（ELF aux vector 的指针/AT_RANDOM 16 字节、
    /// `execve` 时任意 8 位 environ 值）。需要返回原始字节序列。默认
    /// fallback 到 `generate().into_bytes()` 兼容文本生成器。
    fn generate_bytes(&self) -> Vec<u8> {
        self.generate().into_bytes()
    }

    /// 偏移寻址读取（用于 `/proc/[pid]/pagemap` 这类按虚拟页号索引、内容近乎
    /// 无限稀疏、无法整体物化为 `Vec` 的文件）。返回 `Some(n)` 表示已读 n 字节，
    /// `None` 表示该生成器不支持偏移读取 → 调用方回退到 `generate_bytes()` 切片。
    /// Linux `pagemap` 每页 8 字节，offset = vpn*8，必须按需逐项计算。
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> Option<usize> {
        None
    }

    /// 是否可写
    fn writable(&self) -> bool {
        false
    }

    /// 写入处理
    fn write(&self, _data: &str) -> Result<(), ProcError> {
        Err(ProcError::ReadOnly)
    }
}

/// ProcFS inode 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcInodeType {
    /// 目录
    Directory,
    /// 普通文件
    File,
    /// 符号链接
    Symlink,
}

/// ProcFS inode
pub struct ProcInode {
    /// inode 类型
    pub inode_type: ProcInodeType,
    /// 文件名
    pub name: String,
    /// 子节点（目录）
    pub children: Option<BTreeMap<String, Arc<ProcInode>>>,
    /// 内容生成器
    pub generator: Option<Box<dyn ProcFileGenerator>>,
}

impl ProcInode {
    /// 创建目录
    pub fn directory(name: &str) -> Self {
        Self {
            inode_type: ProcInodeType::Directory,
            name: name.to_string(),
            children: Some(BTreeMap::new()),
            generator: None,
        }
    }

    /// 创建文件
    pub fn file(name: &str, generator: Box<dyn ProcFileGenerator>) -> Self {
        Self {
            inode_type: ProcInodeType::File,
            name: name.to_string(),
            children: None,
            generator: Some(generator),
        }
    }

    /// 添加子节点
    pub fn add_child(&mut self, child: Arc<ProcInode>) {
        if let Some(children) = &mut self.children {
            children.insert(child.name.clone(), child);
        }
    }

    /// 查找子节点
    pub fn lookup(&self, name: &str) -> Option<Arc<ProcInode>> {
        self.children.as_ref()?.get(name).cloned()
    }

    /// 读取内容
    pub fn read(&self) -> Result<String, ProcError> {
        match &self.generator {
            Some(gen) => Ok(gen.generate()),
            None => Err(ProcError::NotFound),
        }
    }

    /// Slice 2.5：读取原始字节内容（支持 auxv/environ 等含非 UTF-8 字节
    /// 的文件）。文本生成器 fallback 到 `generate().into_bytes()`。
    pub fn read_bytes(&self) -> Result<Vec<u8>, ProcError> {
        match &self.generator {
            Some(gen) => Ok(gen.generate_bytes()),
            None => Err(ProcError::NotFound),
        }
    }

    /// 偏移寻址读取（pagemap 等稀疏文件）。`Some(n)` = 已读 n 字节；`None` =
    /// 该 inode 不支持偏移读取，调用方回退到 read_bytes 切片。
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> Option<usize> {
        self.generator.as_ref().and_then(|gen| gen.read_at(offset, buf))
    }

    /// 写入内容
    pub fn write(&self, data: &str) -> Result<(), ProcError> {
        match &self.generator {
            Some(gen) => gen.write(data),
            None => Err(ProcError::NotFound),
        }
    }
}

/// ProcFS 文件系统
pub struct ProcFs {
    /// 根目录
    root: Arc<SpinLock<ProcInode>>,
}

impl ProcFs {
    /// 创建新的 ProcFS
    pub fn new() -> Self {
        let mut root = ProcInode::directory("");

        // 添加标准文件
        root.add_child(Arc::new(ProcInode::file("meminfo", Box::new(MeminfoGenerator))));
        root.add_child(Arc::new(ProcInode::file("cpuinfo", Box::new(CpuinfoGenerator))));
        root.add_child(Arc::new(ProcInode::file("version", Box::new(VersionGenerator))));
        root.add_child(Arc::new(ProcInode::file("uptime", Box::new(UptimeGenerator))));
        root.add_child(Arc::new(ProcInode::file("loadavg", Box::new(LoadavgGenerator))));
        root.add_child(Arc::new(ProcInode::file("stat", Box::new(StatGenerator))));
        root.add_child(Arc::new(ProcInode::file("schedstat", Box::new(SchedStatGenerator))));
        root.add_child(Arc::new(ProcInode::file("capabilities", Box::new(CapabilitiesGenerator))));
        root.add_child(Arc::new(ProcInode::file("io_uring", Box::new(IoUringGenerator))));
        root.add_child(Arc::new(ProcInode::file("mounts", Box::new(MountsGenerator))));
        root.add_child(Arc::new(ProcInode::file("interrupts", Box::new(InterruptsGenerator))));
        root.add_child(Arc::new(ProcInode::file("cmdline", Box::new(KernelCmdlineGenerator))));
        root.add_child(Arc::new(ProcInode::file("config.gz", Box::new(KernelConfigGzGenerator))));
        root.add_child(Arc::new(ProcInode::file("filesystems", Box::new(FilesystemsGenerator))));
        root.add_child(Arc::new(ProcInode::file("kpageflags", Box::new(KpageflagsGenerator))));

        root.add_child(Arc::new(crate::fs::proc::sys::build_proc_sys_dir()));

        root.add_child(Arc::new(crate::net::procnet::build_proc_net_dir()));

        // /proc/sysvipc — LTP semctl09 reads /proc/sysvipc/sem
        let mut sysvipc_dir = ProcInode::directory("sysvipc");
        sysvipc_dir.add_child(Arc::new(ProcInode::file("sem", Box::new(SysvipcSemGenerator))));
        sysvipc_dir.add_child(Arc::new(ProcInode::file("msg", Box::new(SysvipcMsgGenerator))));
        sysvipc_dir.add_child(Arc::new(ProcInode::file("shm", Box::new(SysvipcShmGenerator))));
        root.add_child(Arc::new(sysvipc_dir));

        // /proc/self 和 /proc/[pid] 目录由 lookup() 动态生成

        Self {
            root: Arc::new(SpinLock::new(root)),
        }
    }

    /// 查找路径（支持动态 /proc/[pid] 和 /proc/self，以及 Slice 2.5 新增的
    /// `/proc/[pid]/task/[tid]/...` per-thread 视图）
    pub fn lookup(&self, path: &str) -> Option<Arc<ProcInode>> {
        let root = self.root.lock();
        let mut current = Arc::new(ProcInode {
            inode_type: root.inode_type,
            name: root.name.clone(),
            children: root.children.clone(),
            generator: None, // 不能克隆 generator
        });
        drop(root);

        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut i = 0;
        // 记录当前所在 /proc/[pid] 的 tgid，用于校验 task/<tid> 同组关系
        let mut current_tgid: Option<usize> = None;
        while i < components.len() {
            let component = components[i];
            if i == 0 {
                // 根级：处理 "self" 和数字 PID
                let pid = if component == "self" {
                    get_current_pid()
                } else {
                    component.parse::<usize>().ok()
                };
                if let Some(pid) = pid {
                    current = create_pid_dir(pid)?;
                    current_tgid = pid_tgid(pid);
                    i += 1;
                    continue;
                }
            }
            // Slice 2.5 task/<tid> 动态目录：路径形如 /proc/<pid>/task/<tid>...
            // 当上一段是 "task" 且本段是数字时，进 create_tid_dir。要求 tid 与
            // <pid> 同 tgid（与 Linux `/proc/<tgid>/task/<tid>` 语义一致；跨组
            // 访问会 ENOENT）。
            if i >= 2
                && components[i - 1] == "task"
                && component.parse::<usize>().is_ok()
            {
                let tid = component.parse::<usize>().unwrap();
                let want_tgid = current_tgid?;
                let tid_tgid = pid_tgid(tid)?;
                if tid_tgid != want_tgid {
                    return None;
                }
                current = create_tid_dir(tid)?;
                i += 1;
                continue;
            }
            // /proc/[pid]/fdinfo/[fd] — dynamic per-fd info file
            if i >= 2
                && components[i - 1] == "fdinfo"
                && component.parse::<usize>().is_ok()
            {
                let fd_num = component.parse::<usize>().unwrap();
                // Resolve pid from components[0]
                let pid = if components[0] == "self" {
                    get_current_pid()?
                } else {
                    components[0].parse::<usize>().ok()?
                };
                return Some(Arc::new(ProcInode::file(
                    component,
                    Box::new(FdInfoGenerator { pid: pid as u32, fd: fd_num as u32 }),
                )));
            }
            current = current.lookup(component)?;
            i += 1;
        }

        Some(current)
    }

    /// 读取文件
    pub fn read(&self, path: &str) -> Result<String, ProcError> {
        let inode = self.lookup(path).ok_or(ProcError::NotFound)?;
        inode.read()
    }

    /// Slice 2.5：读取文件为原始字节
    pub fn read_bytes(&self, path: &str) -> Result<Vec<u8>, ProcError> {
        let inode = self.lookup(path).ok_or(ProcError::NotFound)?;
        inode.read_bytes()
    }

    /// 偏移寻址读取（pagemap）。inode 不存在或不支持偏移读取时返回 None。
    pub fn read_at(&self, path: &str, offset: usize, buf: &mut [u8]) -> Option<usize> {
        self.lookup(path)?.read_at(offset, buf)
    }

    /// 是否可写
    pub fn writable(&self, path: &str) -> bool {
        match self.lookup(path) {
            Some(inode) => inode.generator.as_ref().map_or(false, |gen| gen.writable()),
            None => false,
        }
    }

    /// 写入内容
    pub fn write(&self, path: &str, data: &str) -> Result<(), ProcError> {
        let inode = self.lookup(path).ok_or(ProcError::NotFound)?;
        inode.write(data)
    }

    /// 列出目录（根目录包含动态 PID 条目；新增 /proc/[pid]/task/<tid> 枚举）
    pub fn readdir(&self, path: &str) -> Result<Vec<String>, ProcError> {
        let trimmed = path.trim_matches('/');
        // 数字 PID dir 或 "self" dir → 返回固定子条目（Slice 2.5 追加 "task"）
        if trimmed == "self" || trimmed.parse::<usize>().is_ok() {
            let pid = if trimmed == "self" {
                get_current_pid()
            } else {
                trimmed.parse::<usize>().ok()
            };
            if pid.and_then(pid_tgid).is_none() {
                return Err(ProcError::NotFound);
            }
            return Ok(alloc::vec![
                "status".into(), "stat".into(), "maps".into(),
                "smaps".into(), "pagemap".into(),
                "cmdline".into(), "comm".into(), "exe".into(),
                "mounts".into(), "mountinfo".into(), "mountstats".into(),
                // Slice 2.5：per-thread 视图
                "task".into(),
                "fd".into(), "fdinfo".into(),
                "io".into(), "limits".into(), "wchan".into(),
                "loginuid".into(), "oom_score".into(),
                // Slice 2.5：oom_score_adj/personality/sessionid/auxv/environ/syscall
                "oom_score_adj".into(), "personality".into(), "coredump_filter".into(),
                "sessionid".into(),
                "timerslack_ns".into(), "auxv".into(), "environ".into(), "syscall".into(),
                // namespace directory
                "ns".into(),
            ]);
        }

        // Slice 2.5: /proc/<pid>/task 目录 → 枚举 tgid 内所有 tid。
        // 与 Linux `ls /proc/<pid>/task` 一致：返回数字 tid 字符串列表。
        let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() == 2 && parts[1] == "task" {
            let pid = match parts[0] {
                "self" => get_current_pid(),
                s => s.parse::<usize>().ok(),
            };
            if let Some(pid) = pid {
                if let Some(tgid) = pid_tgid(pid) {
                    let mut entries = Vec::new();
                    let table = crate::sched::process::PROCESS_TABLE.lock();
                    if let Some(t) = table.as_ref() {
                        for q in t.iter() {
                            if q.tgid == tgid
                                && !matches!(
                                    q.state,
                                    crate::sched::process::ProcessState::Zombie
                                        | crate::sched::process::ProcessState::Unused
                                )
                            {
                                entries.push(format!("{}", q.pid));
                            }
                        }
                    }
                    return Ok(entries);
                }
            }
            return Err(ProcError::NotFound);
        }
        // Slice 2.5: /proc/<pid>/task/<tid> → 返回 per-thread 文件列表
        if parts.len() == 3 && parts[1] == "task" {
            let pid = match parts[0] {
                "self" => get_current_pid(),
                s => s.parse::<usize>().ok(),
            };
            if let (Some(pid), Ok(tid)) = (pid, parts[2].parse::<usize>()) {
                if let (Some(want_tgid), Some(tid_tgid)) = (pid_tgid(pid), pid_tgid(tid)) {
                    if want_tgid == tid_tgid {
                        return Ok(alloc::vec![
                            "status".into(), "stat".into(), "comm".into(),
                            "wchan".into(), "io".into(),
                            // Slice 2.5：per-tid 新增视图
                            "syscall".into(), "oom_score".into(), "oom_score_adj".into(),
                            "personality".into(), "sessionid".into(),
                            "timerslack_ns".into(), "loginuid".into(), "limits".into(),
                            "cmdline".into(), "maps".into(),
                            "exe".into(), "auxv".into(), "environ".into(),
                            "mounts".into(),
                        ]);
                    }
                }
            }
            return Err(ProcError::NotFound);
        }
        // /proc/<pid>/fd → 枚举该进程当前打开的 fd 号（pipe07）。
        if parts.len() == 2 && parts[1] == "fd" {
            let pid = match parts[0] {
                "self" => get_current_pid(),
                s => s.parse::<usize>().ok(),
            };
            if let Some(pid) = pid {
                if let Some(names) = open_fd_names(pid) {
                    return Ok(names);
                }
            }
            return Err(ProcError::NotFound);
        }
        // /proc/<pid>/ns → enumerate namespace types
        if parts.len() == 2 && parts[1] == "ns" {
            return Ok(alloc::vec![
                "cgroup".into(), "ipc".into(), "mnt".into(), "net".into(),
                "pid".into(), "pid_for_children".into(),
                "time".into(), "time_for_children".into(),
                "user".into(), "uts".into(),
            ]);
        }
        // /proc/<pid>/fdinfo → same enumeration as fd
        if parts.len() == 2 && parts[1] == "fdinfo" {
            let pid = match parts[0] {
                "self" => get_current_pid(),
                s => s.parse::<usize>().ok(),
            };
            if let Some(pid) = pid {
                if let Some(names) = open_fd_names(pid) {
                    return Ok(names);
                }
            }
            return Err(ProcError::NotFound);
        }

        let inode = self.lookup(path).ok_or(ProcError::NotFound)?;
        match &inode.children {
            Some(children) => {
                let mut entries: Vec<String> = children.keys().cloned().collect();
                // 根目录：追加 "self" 和所有活跃 PID
                if trimmed.is_empty() {
                    entries.push("self".into());
                    for pid in get_all_pids() {
                        entries.push(format!("{}", pid));
                    }
                }
                Ok(entries)
            },
            None => Err(ProcError::NotFound),
        }
    }
}

impl Default for ProcFs {
    fn default() -> Self {
        Self::new()
    }
}

// ============ 动态 PID 目录辅助函数 — delegated to fs/proc/lookup.rs ============

fn create_pid_dir(pid: usize) -> Option<Arc<ProcInode>> {
    crate::fs::proc::lookup::create_pid_dir(pid)
}

fn create_tid_dir(tid: usize) -> Option<Arc<ProcInode>> {
    crate::fs::proc::lookup::create_tid_dir(tid)
}

// ============ 文件生成器实现 ============

// ============================================================================
// Slice 2.5 真实化：/proc/sys/{kernel,fs,net} 高频节点
// ============================================================================
//
// 这些节点 LTP / glibc / busybox 在各种路径下都会读取；之前缺失会触发
// `open` ENOENT，导致 getrlimit、sysconf、capget、socket listen 等的
// glibc 包装走 fallback 路径甚至直接误判 EINVAL。这里按 Linux 内核
// 真值实现：常量值参考 Linux v5.10 默认（与我们 osrelease 一致）。

/// /proc/[pid]/cmdline 生成器 —— Slice 2.2 真读 PCB `saved_argv`。
///
/// POSIX `proc(5)`：cmdline 是 NUL 分隔的 argv 串，末尾再一个 NUL。
/// 进程已 exit 时 cmdline 为空字符串（Linux 行为）。
/// 全局 ProcFS 实例
static PROCFS: SpinLock<Option<ProcFs>> = SpinLock::new(None);

/// 初始化 ProcFS
pub fn init() {
    let mut procfs = PROCFS.lock();
    *procfs = Some(ProcFs::new());
    crate::println!("[procfs] ProcFS initialized");
}

/// 静默初始化 ProcFS（不输出调试信息）
pub fn init_quiet() {
    let mut procfs = PROCFS.lock();
    *procfs = Some(ProcFs::new());
}

/// 读取 ProcFS 文件
pub fn read(path: &str) -> Result<String, ProcError> {
    PROCFS.lock()
        .as_ref()
        .ok_or(ProcError::NotFound)?
        .read(path)
}

/// 列出 ProcFS 目录
pub fn readdir(path: &str) -> Result<Vec<String>, ProcError> {
    PROCFS.lock()
        .as_ref()
        .ok_or(ProcError::NotFound)?
        .readdir(path)
}

// ============ VFS 集成 — delegated to fs/proc/vfs.rs ============

/// ProcFS VFS 适配器
pub struct ProcFsAdapter {
    pub(crate) procfs: Arc<SpinLock<ProcFs>>,
}

impl ProcFsAdapter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            procfs: Arc::new(SpinLock::new(ProcFs::new())),
        })
    }
}

/// 挂载 ProcFS 到 VFS — delegates to fs/proc/vfs
pub fn mount_procfs() {
    crate::fs::proc::vfs::mount_procfs();
}

/// ProcFS 自测试
pub fn self_test() {
    crate::println!("[procfs] Running self-test...");

    // 测试 ProcError 变体
    let _errors = [
        ProcError::NotFound,
        ProcError::ReadOnly,
        ProcError::InvalidArgument,
        ProcError::PermissionDenied,
    ];

    // 测试 ProcInodeType 变体
    let _types = [
        ProcInodeType::Directory,
        ProcInodeType::File,
        ProcInodeType::Symlink,
    ];

    // 测试 ProcInode 创建
    let mut dir = ProcInode::directory("test_dir");
    let file = ProcInode::file("test_file", Box::new(VersionGenerator));
    dir.add_child(Arc::new(file));

    // 测试查找
    let found = dir.lookup("test_file");
    assert!(found.is_some());

    // 测试读取
    if let Some(inode) = found {
        let content = inode.read();
        assert!(content.is_ok());
    }

    // 测试写入（应该返回 ReadOnly 错误）
    let file2 = ProcInode::file("readonly", Box::new(VersionGenerator));
    let result = file2.write("test");
    assert!(matches!(result, Err(ProcError::ReadOnly)));

    // 测试 ProcFs
    let procfs = ProcFs::new();

    // 测试读取各种文件
    let _meminfo = procfs.read("meminfo");
    let _cpuinfo = procfs.read("cpuinfo");
    let _version = procfs.read("version");
    let _uptime = procfs.read("uptime");
    let _loadavg = procfs.read("loadavg");
    let _stat = procfs.read("stat");
    let _sched_stat = procfs.read("schedstat");
    let _capabilities = procfs.read("capabilities");
    let _io_uring = procfs.read("io_uring");

    // 测试目录列表
    let entries = procfs.readdir("");
    assert!(entries.is_ok());

    // 测试 self 目录
    let _self_status = procfs.read("self/status");
    let _self_maps = procfs.read("self/maps");
    let _self_cmdline = procfs.read("self/cmdline");
    let _self_mounts = procfs.read("self/mounts");
    let _sys_pid_max = procfs.read("sys/kernel/pid_max");
    let _sys_tainted = procfs.read("sys/kernel/tainted");
    let _sys_hostname = procfs.read("sys/kernel/hostname");
    let _sys_osrelease = procfs.read("sys/kernel/osrelease");
    let _sys_suid = procfs.read("sys/fs/suid_dumpable");

    crate::println!("[procfs] Self-test passed!");
}
