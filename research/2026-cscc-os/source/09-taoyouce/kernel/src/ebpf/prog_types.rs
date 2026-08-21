//! eBPF 程序类型
//!
//! 实现 Tracepoint、Kprobe 等 eBPF 程序类型

#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::sync::SpinLock;

/// eBPF 程序类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BpfProgType {
    /// 未指定
    Unspec = 0,
    /// Socket 过滤器
    SocketFilter = 1,
    /// Kprobe
    Kprobe = 2,
    /// 调度器类
    SchedCls = 3,
    /// 调度器动作
    SchedAct = 4,
    /// Tracepoint
    Tracepoint = 5,
    /// XDP
    Xdp = 6,
    /// Perf 事件
    PerfEvent = 7,
    /// Cgroup SKB
    CgroupSkb = 8,
    /// Cgroup Sock
    CgroupSock = 9,
    /// LWT In
    LwtIn = 10,
    /// LWT Out
    LwtOut = 11,
    /// LWT Xmit
    LwtXmit = 12,
    /// Sock Ops
    SockOps = 13,
    /// SK SKB
    SkSkb = 14,
    /// Cgroup Device
    CgroupDevice = 15,
    /// SK Msg
    SkMsg = 16,
    /// Raw Tracepoint
    RawTracepoint = 17,
    /// Cgroup Sock Addr
    CgroupSockAddr = 18,
    /// LWT Seg6local
    LwtSeg6local = 19,
    /// Lirc Mode2
    LircMode2 = 20,
    /// SK Reuseport
    SkReuseport = 21,
    /// Flow Dissector
    FlowDissector = 22,
    /// Cgroup Sysctl
    CgroupSysctl = 23,
    /// Raw Tracepoint Writable
    RawTracepointWritable = 24,
    /// Cgroup Sockopt
    CgroupSockopt = 25,
    /// Tracing
    Tracing = 26,
    /// Struct Ops
    StructOps = 27,
    /// Ext
    Ext = 28,
    /// LSM
    Lsm = 29,
    /// SK Lookup
    SkLookup = 30,
}

/// eBPF 附加类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BpfAttachType {
    /// Cgroup Inet Ingress
    CgroupInetIngress = 0,
    /// Cgroup Inet Egress
    CgroupInetEgress = 1,
    /// Cgroup Inet Sock Create
    CgroupInetSockCreate = 2,
    /// Cgroup Sock Ops
    CgroupSockOps = 3,
    /// SK SKB Stream Parser
    SkSkbStreamParser = 4,
    /// SK SKB Stream Verdict
    SkSkbStreamVerdict = 5,
    /// Cgroup Device
    CgroupDevice = 6,
    /// SK Msg Verdict
    SkMsgVerdict = 7,
    /// Cgroup Inet4 Bind
    CgroupInet4Bind = 8,
    /// Cgroup Inet6 Bind
    CgroupInet6Bind = 9,
    /// Cgroup Inet4 Connect
    CgroupInet4Connect = 10,
    /// Cgroup Inet6 Connect
    CgroupInet6Connect = 11,
    /// Cgroup Inet4 Post Bind
    CgroupInet4PostBind = 12,
    /// Cgroup Inet6 Post Bind
    CgroupInet6PostBind = 13,
    /// Cgroup UDP4 Sendmsg
    CgroupUdp4Sendmsg = 14,
    /// Cgroup UDP6 Sendmsg
    CgroupUdp6Sendmsg = 15,
    /// Lirc Mode2
    LircMode2 = 16,
    /// Flow Dissector
    FlowDissector = 17,
    /// Cgroup Sysctl
    CgroupSysctl = 18,
    /// Cgroup UDP4 Recvmsg
    CgroupUdp4Recvmsg = 19,
    /// Cgroup UDP6 Recvmsg
    CgroupUdp6Recvmsg = 20,
    /// Cgroup Getsockopt
    CgroupGetsockopt = 21,
    /// Cgroup Setsockopt
    CgroupSetsockopt = 22,
    /// Trace Raw TP
    TraceRawTp = 23,
    /// Trace Fentry
    TraceFentry = 24,
    /// Trace Fexit
    TraceFexit = 25,
    /// Modify Return
    ModifyReturn = 26,
    /// LSM Mac
    LsmMac = 27,
    /// Trace Iter
    TraceIter = 28,
    /// Cgroup Inet4 Getpeername
    CgroupInet4Getpeername = 29,
    /// Cgroup Inet6 Getpeername
    CgroupInet6Getpeername = 30,
    /// Cgroup Inet4 Getsockname
    CgroupInet4Getsockname = 31,
    /// Cgroup Inet6 Getsockname
    CgroupInet6Getsockname = 32,
    /// XDP Devmap
    XdpDevmap = 33,
    /// Cgroup Inet Sock Release
    CgroupInetSockRelease = 34,
    /// XDP Cpumap
    XdpCpumap = 35,
    /// SK Lookup
    SkLookup = 36,
    /// XDP
    Xdp = 37,
}

/// Kprobe 上下文
#[repr(C)]
pub struct KprobeContext {
    /// 程序计数器
    pub pc: u64,
    /// 返回地址
    pub ra: u64,
    /// 栈指针
    pub sp: u64,
    /// 参数寄存器
    pub args: [u64; 8],
    /// 返回值（仅 kretprobe）
    pub ret: u64,
}

impl KprobeContext {
    pub fn new() -> Self {
        Self {
            pc: 0,
            ra: 0,
            sp: 0,
            args: [0; 8],
            ret: 0,
        }
    }
}

/// Tracepoint 上下文
#[repr(C)]
pub struct TracepointContext {
    /// 事件类型
    pub event_type: u32,
    /// 事件标志
    pub flags: u32,
    /// 时间戳
    pub timestamp: u64,
    /// CPU ID
    pub cpu: u32,
    /// 进程 ID
    pub pid: u32,
    /// 线程 ID
    pub tid: u32,
    /// 保留
    pub reserved: u32,
    /// 数据指针
    pub data: u64,
    /// 数据长度
    pub data_len: u32,
}

impl TracepointContext {
    pub fn new(event_type: u32) -> Self {
        Self {
            event_type,
            flags: 0,
            timestamp: 0,
            cpu: 0,
            pid: 0,
            tid: 0,
            reserved: 0,
            data: 0,
            data_len: 0,
        }
    }
}

/// Kprobe 处理器
pub struct KprobeHandler {
    /// 探测点地址
    pub addr: u64,
    /// 符号名称
    pub symbol: String,
    /// 是否为返回探测
    pub is_return: bool,
    /// eBPF 程序 ID
    pub prog_id: u32,
    /// 是否启用
    pub enabled: AtomicBool,
    /// 触发次数
    pub hit_count: AtomicU64,
}

impl KprobeHandler {
    pub fn new(addr: u64, symbol: String, is_return: bool, prog_id: u32) -> Self {
        Self {
            addr,
            symbol,
            is_return,
            prog_id,
            enabled: AtomicBool::new(true),
            hit_count: AtomicU64::new(0),
        }
    }
    
    /// 处理探测点命中
    pub fn handle(&self, ctx: &mut KprobeContext) -> i32 {
        if !self.enabled.load(Ordering::Acquire) {
            return 0;
        }
        
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        
        // 调用 eBPF 程序
        // 实际实现需要调用 JIT 编译的代码
        let _ = ctx;
        0
    }
    
    /// 启用
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }
    
    /// 禁用
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }
}

/// Tracepoint 定义
pub struct TracepointDef {
    /// 类别
    pub category: String,
    /// 名称
    pub name: String,
    /// 格式字符串
    pub format: String,
    /// 字段定义
    pub fields: Vec<TracepointField>,
}

/// Tracepoint 字段
pub struct TracepointField {
    /// 字段名
    pub name: String,
    /// 字段类型
    pub field_type: String,
    /// 偏移
    pub offset: usize,
    /// 大小
    pub size: usize,
}

/// Tracepoint 处理器
pub struct TracepointHandler {
    /// Tracepoint 定义
    pub def: TracepointDef,
    /// eBPF 程序 ID
    pub prog_id: u32,
    /// 是否启用
    pub enabled: AtomicBool,
    /// 触发次数
    pub hit_count: AtomicU64,
}

impl TracepointHandler {
    pub fn new(def: TracepointDef, prog_id: u32) -> Self {
        Self {
            def,
            prog_id,
            enabled: AtomicBool::new(true),
            hit_count: AtomicU64::new(0),
        }
    }
    
    /// 处理 tracepoint 触发
    pub fn handle(&self, ctx: &mut TracepointContext) -> i32 {
        if !self.enabled.load(Ordering::Acquire) {
            return 0;
        }
        
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        
        // 调用 eBPF 程序
        let _ = ctx;
        0
    }
}

/// Perf 事件处理器
pub struct PerfEventHandler {
    /// 事件类型
    pub event_type: u32,
    /// 事件配置
    pub config: u64,
    /// 采样周期
    pub sample_period: u64,
    /// eBPF 程序 ID
    pub prog_id: u32,
    /// 是否启用
    pub enabled: AtomicBool,
}

impl PerfEventHandler {
    pub fn new(event_type: u32, config: u64, sample_period: u64, prog_id: u32) -> Self {
        Self {
            event_type,
            config,
            sample_period,
            prog_id,
            enabled: AtomicBool::new(true),
        }
    }
}

/// 程序类型管理器
pub struct ProgTypeManager {
    /// Kprobe 处理器
    kprobes: BTreeMap<u64, KprobeHandler>,
    /// Tracepoint 处理器
    tracepoints: BTreeMap<String, TracepointHandler>,
    /// Perf 事件处理器
    perf_events: Vec<PerfEventHandler>,
    /// 下一个程序 ID
    next_prog_id: u32,
}

impl ProgTypeManager {
    pub fn new() -> Self {
        Self {
            kprobes: BTreeMap::new(),
            tracepoints: BTreeMap::new(),
            perf_events: Vec::new(),
            next_prog_id: 1,
        }
    }
    
    /// 注册 Kprobe
    pub fn register_kprobe(&mut self, addr: u64, symbol: &str, is_return: bool) -> u32 {
        let prog_id = self.next_prog_id;
        self.next_prog_id += 1;
        
        let handler = KprobeHandler::new(addr, String::from(symbol), is_return, prog_id);
        self.kprobes.insert(addr, handler);
        
        crate::println!("[ebpf] Registered kprobe at {:#x} ({})", addr, symbol);
        prog_id
    }
    
    /// 注销 Kprobe
    pub fn unregister_kprobe(&mut self, addr: u64) -> Result<(), &'static str> {
        self.kprobes.remove(&addr).ok_or("Kprobe not found")?;
        Ok(())
    }
    
    /// 注册 Tracepoint
    pub fn register_tracepoint(&mut self, category: &str, name: &str) -> u32 {
        let prog_id = self.next_prog_id;
        self.next_prog_id += 1;
        
        let def = TracepointDef {
            category: String::from(category),
            name: String::from(name),
            format: String::new(),
            fields: Vec::new(),
        };
        
        let handler = TracepointHandler::new(def, prog_id);
        let key = alloc::format!("{}:{}", category, name);
        self.tracepoints.insert(key.clone(), handler);
        
        crate::println!("[ebpf] Registered tracepoint {}", key);
        prog_id
    }
    
    /// 触发 Kprobe
    pub fn trigger_kprobe(&self, addr: u64, ctx: &mut KprobeContext) -> i32 {
        if let Some(handler) = self.kprobes.get(&addr) {
            handler.handle(ctx)
        } else {
            0
        }
    }
    
    /// 触发 Tracepoint
    pub fn trigger_tracepoint(&self, category: &str, name: &str, ctx: &mut TracepointContext) -> i32 {
        let key = alloc::format!("{}:{}", category, name);
        if let Some(handler) = self.tracepoints.get(&key) {
            handler.handle(ctx)
        } else {
            0
        }
    }
    
    /// 获取 Kprobe 统计
    pub fn get_kprobe_stats(&self, addr: u64) -> Option<u64> {
        self.kprobes.get(&addr).map(|h| h.hit_count.load(Ordering::Relaxed))
    }
    
    /// 列出所有 Kprobe
    pub fn list_kprobes(&self) -> Vec<(u64, String, bool, u64)> {
        self.kprobes.iter().map(|(addr, h)| {
            (*addr, h.symbol.clone(), h.is_return, h.hit_count.load(Ordering::Relaxed))
        }).collect()
    }
    
    /// 列出所有 Tracepoint
    pub fn list_tracepoints(&self) -> Vec<(String, u64)> {
        self.tracepoints.iter().map(|(key, h)| {
            (key.clone(), h.hit_count.load(Ordering::Relaxed))
        }).collect()
    }
}

/// 全局程序类型管理器
static PROG_TYPE_MANAGER: SpinLock<Option<ProgTypeManager>> = SpinLock::new(None);

/// 初始化程序类型管理器
pub fn init() {
    *PROG_TYPE_MANAGER.lock() = Some(ProgTypeManager::new());
    
    // 注册内置 tracepoint
    register_builtin_tracepoints();
    
    crate::println!("[ebpf] Program type manager initialized");
}

/// 注册内置 tracepoint
fn register_builtin_tracepoints() {
    let mut mgr = PROG_TYPE_MANAGER.lock();
    if let Some(ref mut m) = *mgr {
        // 调度器 tracepoint
        m.register_tracepoint("sched", "sched_switch");
        m.register_tracepoint("sched", "sched_wakeup");
        m.register_tracepoint("sched", "sched_process_fork");
        m.register_tracepoint("sched", "sched_process_exit");
        
        // 系统调用 tracepoint
        m.register_tracepoint("syscalls", "sys_enter");
        m.register_tracepoint("syscalls", "sys_exit");
        
        // 内存 tracepoint
        m.register_tracepoint("kmem", "kmalloc");
        m.register_tracepoint("kmem", "kfree");
        m.register_tracepoint("kmem", "page_alloc");
        m.register_tracepoint("kmem", "page_free");
        
        // 中断 tracepoint
        m.register_tracepoint("irq", "irq_handler_entry");
        m.register_tracepoint("irq", "irq_handler_exit");
    }
}

/// 注册 Kprobe
pub fn register_kprobe(addr: u64, symbol: &str, is_return: bool) -> Option<u32> {
    PROG_TYPE_MANAGER.lock()
        .as_mut()
        .map(|m| m.register_kprobe(addr, symbol, is_return))
}

/// 触发 Kprobe
pub fn trigger_kprobe(addr: u64, ctx: &mut KprobeContext) -> i32 {
    PROG_TYPE_MANAGER.lock()
        .as_ref()
        .map(|m| m.trigger_kprobe(addr, ctx))
        .unwrap_or(0)
}

/// 触发 Tracepoint
pub fn trigger_tracepoint(category: &str, name: &str, ctx: &mut TracepointContext) -> i32 {
    PROG_TYPE_MANAGER.lock()
        .as_ref()
        .map(|m| m.trigger_tracepoint(category, name, ctx))
        .unwrap_or(0)
}

/// 宏：触发调度器 tracepoint
#[macro_export]
macro_rules! trace_sched_switch {
    ($prev_pid:expr, $next_pid:expr) => {{
        let mut ctx = $crate::ebpf::prog_types::TracepointContext::new(0);
        ctx.pid = $prev_pid;
        ctx.tid = $next_pid;
        $crate::ebpf::prog_types::trigger_tracepoint("sched", "sched_switch", &mut ctx);
    }};
}

/// 宏：触发系统调用 tracepoint
#[macro_export]
macro_rules! trace_syscall_enter {
    ($syscall_nr:expr, $args:expr) => {{
        let mut ctx = $crate::ebpf::prog_types::TracepointContext::new($syscall_nr);
        ctx.data = $args as u64;
        $crate::ebpf::prog_types::trigger_tracepoint("syscalls", "sys_enter", &mut ctx);
    }};
}
