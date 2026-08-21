//! 多核 SMP 支持
//!
//! 实现对称多处理器支持：hart 在线状态机、AP 启动与 IPI。
//! 运行队列唯一权威在 `cpu_rq`，负载均衡/选核策略在 `policy`。

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::sync::SpinLock;

/// 最大 CPU 数量（统一 per-CPU 底座；决赛 RV=8 / LA=12，预留 16）。
pub use crate::percpu::MAX_CPUS;

/// CPU 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuState {
    /// 离线
    Offline,
    /// 启动中
    Booting,
    /// 在线
    Online,
    /// 空闲
    Idle,
}

/// 每 CPU 平台状态（启动/在线/离线）。
///
/// 注意：这里**不再**持有运行队列。生产调度队列唯一权威是
/// `sched::cpu_rq::PERCPU_RQ`，避免两套队列状态漂移。need_resched 的
/// 唯一权威是 `sched::process_sched::NEED_RESCHED`。
pub struct PerCpu {
    /// CPU ID
    pub cpu_id: usize,
    /// CPU 状态
    pub state: CpuState,
}

impl PerCpu {
    pub fn new(cpu_id: usize) -> Self {
        Self {
            cpu_id,
            state: CpuState::Offline,
        }
    }
}

/// CPU 亲和性掩码
#[derive(Clone, Copy)]
pub struct CpuMask(pub u64);

impl CpuMask {
    pub const ALL: CpuMask = CpuMask(u64::MAX);
    
    pub fn new() -> Self {
        Self(0)
    }
    
    pub fn single(cpu: usize) -> Self {
        Self(1 << cpu)
    }
    
    pub fn set(&mut self, cpu: usize) {
        self.0 |= 1 << cpu;
    }
    
    pub fn clear(&mut self, cpu: usize) {
        self.0 &= !(1 << cpu);
    }
    
    pub fn is_set(&self, cpu: usize) -> bool {
        (self.0 & (1 << cpu)) != 0
    }
    
    pub fn first_set(&self) -> Option<usize> {
        if self.0 == 0 {
            None
        } else {
            Some(self.0.trailing_zeros() as usize)
        }
    }
    
    pub fn count(&self) -> usize {
        self.0.count_ones() as usize
    }
}

/// 全局 SMP 管理器
static SMP_MANAGER: SpinLock<Option<SmpManager>> = SpinLock::new(None);

/// SMP 管理器
pub struct SmpManager {
    /// 每 CPU 数据
    per_cpu: Vec<SpinLock<PerCpu>>,
    /// 在线 CPU 数量
    online_cpus: AtomicUsize,
}

impl SmpManager {
    pub fn new(num_cpus: usize, boot_cpu_id: usize) -> Self {
        let mut per_cpu = Vec::with_capacity(num_cpus);
        for i in 0..num_cpus {
            per_cpu.push(SpinLock::new(PerCpu::new(i)));
        }
        
        let manager = Self {
            per_cpu,
            online_cpus: AtomicUsize::new(0),
        };

        if boot_cpu_id < num_cpus {
            let mut cpu = manager.per_cpu[boot_cpu_id].lock();
            cpu.state = CpuState::Online;
            manager.online_cpus.store(1, Ordering::SeqCst);
        }

        manager
    }
    
    /// 启动 AP (Application Processor)
    pub fn boot_ap(&self, cpu_id: usize) -> Result<(), &'static str> {
        if cpu_id >= self.per_cpu.len() {
            crate::println!("[smp] boot_ap reject hart {}: invalid, num_cpus={}", cpu_id, self.per_cpu.len());
            return Err("Invalid CPU ID");
        }
        
        let mut cpu = self.per_cpu[cpu_id].lock();
        crate::println!("[smp] boot_ap request hart {} state {:?}", cpu_id, cpu.state);
        if cpu.state != CpuState::Offline {
            return Err("CPU already online");
        }
        
        cpu.state = CpuState::Booting;
        drop(cpu);

        match self.boot_arch_ap(cpu_id) {
            Ok(()) => Ok(()),
            Err(e) => {
                let mut cpu = self.per_cpu[cpu_id].lock();
                if cpu.state == CpuState::Booting {
                    cpu.state = CpuState::Offline;
                }
                Err(e)
            }
        }
    }
    
    #[cfg(target_arch = "riscv64")]
    fn boot_arch_ap(&self, cpu_id: usize) -> Result<(), &'static str> {
        unsafe {
            extern "C" {
                fn secondary_hart_start();
            }
            let ret: isize;
            let entry = secondary_hart_start as usize;
            crate::println!("[smp] sbi hart_start hart {} entry {:#x}", cpu_id, entry);
            core::arch::asm!(
                "li a7, 0x48534d",
                "li a6, 0",
                "ecall",
                in("a0") cpu_id,
                in("a1") entry,
                in("a2") 0usize,
                lateout("a0") ret,
                out("a6") _,
                out("a7") _,
            );
            crate::println!("[smp] sbi hart_start hart {} ret {}", cpu_id, ret);
            if ret == 0 || ret == -6 {
                Ok(())
            } else {
                Err("SBI hart_start failed")
            }
        }
    }

    #[cfg(target_arch = "loongarch64")]
    fn boot_arch_ap(&self, cpu_id: usize) -> Result<(), &'static str> {
        use crate::arch::loongarch64::{iocsr, mm};

        if cpu_id >= MAX_CPUS {
            return Err("LoongArch CPU ID exceeds AP stack capacity");
        }
        extern "C" {
            fn secondary_hart_start();
        }
        let entry = mm::kern_vaddr_to_phys(secondary_hart_start as usize);
        let send_mailbox = |box_id: u32, value: u32| {
            let command = iocsr::MBUF_SEND_BLOCKING
                | ((box_id << iocsr::MBUF_SEND_BOX_SHIFT) as u64)
                | ((cpu_id as u64) << iocsr::MBUF_SEND_CPU_SHIFT)
                | ((value as u64) << iocsr::MBUF_SEND_BUF_SHIFT);
            unsafe { iocsr::write64(iocsr::MBUF_SEND, command) };
        };
        send_mailbox(iocsr::MBUF_SEND_BOX_HI, (entry >> 32) as u32);
        send_mailbox(iocsr::MBUF_SEND_BOX_LO, entry as u32);
        // IPI_SEND[4:0] 为位索引（Linux ipi_write_action 语义）。
        unsafe { iocsr::send_ipi_action(cpu_id, iocsr::ACTION_BOOT_CPU) };
        Ok(())
    }

    /// AP 启动完成回调
    pub fn ap_online(&self, cpu_id: usize) {
        if cpu_id < self.per_cpu.len() {
            let previous = {
                let mut cpu = self.per_cpu[cpu_id].lock();
                let previous = cpu.state;
                if cpu.state != CpuState::Online && cpu.state != CpuState::Idle {
                    self.online_cpus.fetch_add(1, Ordering::SeqCst);
                    ONLINE_COUNT.fetch_add(1, Ordering::Relaxed);
                    if cpu_id < 64 {
                        ONLINE_MASK.fetch_or(1u64 << cpu_id, Ordering::Relaxed);
                    }
                }
                cpu.state = CpuState::Online;
                previous
            };
            crate::println!("[smp] ap_online hart {} previous {:?} online_cpus {} mask {:#x}", cpu_id, previous, self.num_online(), self.online_mask());
        } else {
            crate::println!("[smp] ap_online ignored hart {}: num_cpus={}", cpu_id, self.per_cpu.len());
        }
    }
    
    /// 获取在线 CPU 数量
    pub fn num_online(&self) -> usize {
        self.online_cpus.load(Ordering::SeqCst)
    }

    pub fn online_mask(&self) -> u64 {
        let mut mask = 0u64;
        for i in 0..self.per_cpu.len().min(MAX_CPUS).min(u64::BITS as usize) {
            let cpu = self.per_cpu[i].lock();
            if cpu.state == CpuState::Online || cpu.state == CpuState::Idle {
                mask |= 1u64 << i;
            }
        }
        mask
    }
    
    /// 选择亲和性掩码内负载最轻的在线 CPU。
    ///
    /// 负载读数使用 `cpu_rq` 的无锁镜像（`rq_load`），不锁任何 rq；生产
    /// 选核主路径在 `sched::policy::select_task_rq`，本方法仅作兼容/诊断入口。
    pub fn select_cpu(&self, affinity: CpuMask) -> usize {
        let mut best = 0usize;
        let mut best_load = u64::MAX;
        for i in 0..self.per_cpu.len().min(MAX_CPUS) {
            if !affinity.is_set(i) {
                continue;
            }
            let cpu = self.per_cpu[i].lock();
            if cpu.state != CpuState::Online && cpu.state != CpuState::Idle {
                continue;
            }
            drop(cpu);
            let load = crate::sched::cpu_rq::rq_load(i);
            if load < best_load {
                best_load = load;
                best = i;
            }
        }
        best
    }

    /// 发送处理器间中断
    pub fn send_ipi(&self, cpu_id: usize, ipi_type: IpiType) {
        #[cfg(target_arch = "riscv64")]
        {
            let hart_mask = 1usize << cpu_id;

            unsafe {
                let _ret: usize;
                match ipi_type {
                    IpiType::Reschedule | IpiType::Init | IpiType::CallFunction | IpiType::Stop => {
                        core::arch::asm!(
                            "li a7, 0x735049",
                            "li a6, 0",
                            "ecall",
                            in("a0") hart_mask,
                            in("a1") 0usize,
                            lateout("a0") _ret,
                            out("a6") _,
                            out("a7") _,
                        );
                    }
                }
            }
        }

        #[cfg(target_arch = "loongarch64")]
        {
            use crate::arch::loongarch64::iocsr;
            let action = match ipi_type {
                IpiType::Reschedule => iocsr::ACTION_RESCHEDULE,
                IpiType::Init => iocsr::ACTION_BOOT_CPU,
                IpiType::CallFunction => iocsr::ACTION_FUNCTION_CALL,
                IpiType::Stop => iocsr::ACTION_STOP,
            };
            // IPI_SEND[4:0] 为位索引（Linux ipi_write_action 语义）。
            unsafe { iocsr::send_ipi_action(cpu_id, action) };
        }
    }
    
    /// 广播 IPI 到所有其他 CPU
    pub fn send_ipi_all(&self, ipi_type: IpiType) {
        let current = current_cpu();
        for i in 0..self.per_cpu.len() {
            if i != current {
                self.send_ipi(i, ipi_type);
            }
        }
    }
}

/// IPI 类型
#[derive(Clone, Copy)]
pub enum IpiType {
    /// 重新调度
    Reschedule,
    /// 初始化
    Init,
    /// 调用函数
    CallFunction,
    /// 停止
    Stop,
}

/// 向指定 hart 发一次 reschedule IPI，把它从 `wfi`/用户态踢进内核。
///
/// **不取任何锁**：调用点（如 `zap_other_threads` 语义的组退出）已持
/// `PROCESS_TABLE`，再去抢 `SMP_MANAGER` 会形成跨核锁环（GDB 取证
/// `rv-smpgdb-20260811T021540Z.gdb.txt` 就是这类环）。IPI 发送本身只写
/// 架构寄存器 / 走 SBI，不需要管理器状态。
pub fn kick_hart(hart: usize) {
    if hart >= MAX_CPUS || hart == current_cpu() {
        return;
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        let hart_mask = 1usize << hart;
        let _ret: usize;
        core::arch::asm!(
            "li a7, 0x735049",
            "li a6, 0",
            "ecall",
            in("a0") hart_mask,
            in("a1") 0usize,
            lateout("a0") _ret,
            out("a6") _,
            out("a7") _,
        );
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        use crate::arch::loongarch64::iocsr;
        // IPI_SEND[4:0] 为位索引：旧的掩码写法把 RESCHEDULE(0x2) 置到对端
        // status 的 bit2（shootdown 位），被误当成 TLB shootdown 处理。
        iocsr::send_ipi_action(hart, iocsr::ACTION_RESCHEDULE);
    }
}

/// 初始化 SMP
pub fn init(num_cpus: usize, boot_cpu_id: usize) {
    let boot = boot_cpu_id.min(MAX_CPUS.saturating_sub(1));
    BOOT_HART.store(boot, Ordering::Relaxed);
    ONLINE_MASK.store(1u64 << boot, Ordering::Relaxed);
    ONLINE_COUNT.store(1, Ordering::Relaxed);
    let mut mgr = SMP_MANAGER.lock();
    *mgr = Some(SmpManager::new(num_cpus, boot_cpu_id));
    set_current_cpu(boot_cpu_id);
    
    if let Some(ref m) = *mgr {
        crate::println!("[smp] SMP initialized with {} CPUs, boot_hart={}, online_cpus={}, online_mask={:#x}", num_cpus, boot_cpu_id, m.num_online(), m.online_mask());
    }
}

/// 锁内结构之上还有一个**无锁镜像**：`ONLINE_COUNT` 原子在线核数，供
/// mark_runnable/kick 等「已持 PROCESS_TABLE」的路径读在线核数而不取
/// SMP_MANAGER——持 PT 再取 SMP_MANAGER 与「AP 在 ap_online 持 SMP_MANAGER
/// 打印时被 tick 中断、ISR 抢 PT」构成 ABBA 死锁（2026-08-15 RV SMP=2 init
/// 挂死，SPINDBG+GDB 取证实锤）。任何热路径读核数一律走 num_online_fast。
static ONLINE_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(1);
static ONLINE_MASK: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
static BOOT_HART: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// 无锁在线核数（kick/热路径专用；唯一写点是 ap_online 的 +1）。
#[inline]
pub fn num_online_fast() -> usize {
    ONLINE_COUNT.load(core::sync::atomic::Ordering::Relaxed)
}

/// 无锁在线 hart 位图。下标是硬件 hart id（BSP 可能是 4）。
#[inline]
pub fn online_mask_fast() -> u64 {
    let mask = ONLINE_MASK.load(core::sync::atomic::Ordering::Relaxed);
    if mask == 0 {
        1
    } else {
        mask
    }
}

#[inline]
pub fn boot_hart() -> usize {
    BOOT_HART.load(core::sync::atomic::Ordering::Relaxed)
}

/// 获取在线 CPU 数量
pub fn num_online() -> usize {
    SMP_MANAGER.lock()
        .as_ref()
        .map(|m| m.num_online())
        .unwrap_or(1)
}

/// 获取在线 CPU 掩码
pub fn online_mask() -> u64 {
    SMP_MANAGER.lock()
        .as_ref()
        .map(|m| m.online_mask())
        .unwrap_or(1)
}

/// 获取当前 CPU ID。硬件 hart ID 才是 per-CPU 真值；全局原子会被最后上线的
/// AP 覆盖，导致所有 hart 把 IPI/调度操作归到同一个 CPU。
pub fn current_cpu() -> usize {
    crate::arch::asm_helpers::read_cpu_id()
}

/// 设置当前 CPU ID。RISC-V 将 ID 放入 tp；LoongArch 从只读 CPUID CSR 获取。
pub fn set_current_cpu(cpu_id: usize) {
    crate::arch::asm_helpers::write_cpu_id(cpu_id);
}

/// 启动 AP
pub fn boot_ap(cpu_id: usize) -> Result<(), &'static str> {
    SMP_MANAGER.lock()
        .as_ref()
        .ok_or("SMP not initialized")?
        .boot_ap(cpu_id)
}

/// 选择 CPU（兼容/诊断入口；生产选核走 `sched::policy::select_task_rq`）。
pub fn select_cpu(affinity: CpuMask) -> usize {
    SMP_MANAGER.lock()
        .as_ref()
        .map(|m| m.select_cpu(affinity))
        .unwrap_or(0)
}

/// 通知 AP 上线
pub fn ap_online(cpu_id: usize) {
    if let Some(ref mgr) = *SMP_MANAGER.lock() {
        mgr.ap_online(cpu_id);
    }
}

/// SMP 自测试
pub fn self_test() {
    crate::println!("[smp] Running self-test...");
    
    // 测试 MAX_CPUS 常量
    assert_eq!(MAX_CPUS, 16);
    
    // 测试 SmpManager
    let mgr = SmpManager::new(4, 0);
    
    // 测试 online_cpus 字段
    let online = mgr.online_cpus.load(Ordering::SeqCst);
    assert_eq!(online, 1); // BSP 已在线
    
    // 测试 ap_online
    mgr.ap_online(1);
    let online = mgr.online_cpus.load(Ordering::SeqCst);
    assert_eq!(online, 2);
    
    // 测试 num_online
    let num = mgr.num_online();
    assert_eq!(num, 2);
    
    // 测试 send_ipi_all（不会真正发送，因为没有其他 CPU）
    // 我们只验证函数存在
    let _send_ipi_all_fn = |mgr: &SmpManager, ipi: IpiType| mgr.send_ipi_all(ipi);
    
    crate::println!("[smp] Self-test passed!");
}
