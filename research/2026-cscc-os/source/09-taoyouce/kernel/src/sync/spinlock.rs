//! 自旋锁实现

#![allow(dead_code)]

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// 自旋锁争用统计（**仅慢路径**累加，无争用时零成本）。
///
/// 用来回答"多核退化的时间到底耗在哪"：`CONTENDED` 是发生争用的加锁次数，
/// `SPIN_ROUNDS` 是累计自旋轮数。由 HUNGDBG 周期打印，配合 shootdown 计数即可
/// 区分「锁争用」「跨核 TLB 同步等待」「其它」。
pub static LOCK_CONTENDED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
pub static LOCK_SPIN_ROUNDS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
/// 保留计数位（park 机制已回滚，恒为 0；输出格式保持稳定便于跨批次对比）。
pub static LOCK_PARKS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// 自旋锁（TTAS）。
///
/// 曾尝试过 paravirt 风格的 park/kick（自旋超阈值就 `wfi` 让出物理核、持锁者
/// `unlock` 时发 IPI 唤醒），动机是 QEMU TCG 下 `pause` 不让出物理核会放大
/// lock holder preemption。**已回滚**，两点原因：
/// 1. 算账否掉了收益：实测 `lock_contended=4906`、`lock_spins=50828894`，
///    约 5×10^7 轮自旋合计不过亚秒级，而退化是分钟级——锁争用不是主因；
/// 2. 引入后 init 阶段直接 `fork_failed`（功能破坏），不值得为非主因承担风险。
///
/// 保留 TTAS（抢不到时只读自旋，不反复写 CAS 抢缓存行独占）与争用计数。
///
/// 诊断增强（SPINDBG=1 门控的争用打印之外恒开）：记录最近一次成功持锁者的
/// 返回地址与 pid——死锁/活锁现场只要再等几秒，争用者就能把持有者 RA 打出来，
/// 不用猜「这把锁到底在谁手里」（2026-08-15 RV SMP=2 init 挂死取证）。
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
    holder_ra: core::sync::atomic::AtomicUsize,
    holder_pid: core::sync::atomic::AtomicUsize,
}

/// 自旋锁守卫
pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

// SpinLock 可以安全地在线程间共享
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// 最近成功持锁者的返回地址 / pid（诊断用）。
    pub fn holder(&self) -> (usize, usize) {
        (
            self.holder_ra.load(Ordering::Relaxed),
            self.holder_pid.load(Ordering::Relaxed),
        )
    }
}

impl<T> SpinLock<T> {
    /// 创建新的自旋锁
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
            holder_ra: core::sync::atomic::AtomicUsize::new(0),
            holder_pid: core::sync::atomic::AtomicUsize::new(0),
        }
    }
    
    /// 获取锁。
    ///
    /// TTAS（test-and-test-and-set）：抢不到时**只读**自旋，看起来空闲了再回去
    /// 做一次 CAS。对标 Linux 的 `queued_spinlock`/早期 `ticket_spinlock` 所解决
    /// 的同一问题——纯 TAS 会让每个等待者反复发起写（独占）请求，N 个核把同一条
    /// 缓存行来回抢（cacheline ping-pong），争用越高吞吐越低。
    ///
    /// 在 QEMU TCG 下这一点被极度放大：原子读改写要走全局慢路径，比普通读贵几个
    /// 数量级。实测（8 核 `cargo build`，`HUNGDBG` 周期采样）：改前整机
    /// **每 8 秒只完成 3 个 syscall**，CPU 却稳定 576%——几乎全部时间耗在 CAS
    /// 争用上，而进程确实在缓慢推进（rustc Running→Zombie），即不是死锁。
    #[inline]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        // 本函数的返回地址 = 锁的请求者（如 num_online），记录在案。
        let caller_ra: usize;
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("mv {}, ra", out(reg) caller_ra, options(nomem, nostack)); }
        #[cfg(target_arch = "loongarch64")]
        unsafe { core::arch::asm!("move {}, $ra", out(reg) caller_ra, options(nomem, nostack)); }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
        { caller_ra = 0; }
        let mut reported = false;
        loop {
            // 快路径：无争用时一次 CAS 拿到。
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                self.holder_ra.store(caller_ra, Ordering::Relaxed);
                self.holder_pid.store(
                    crate::sched::process_sched::HART_CURRENT_PID
                        .load(core::sync::atomic::Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                return SpinLockGuard { lock: self };
            }
            // 慢路径：纯读等待锁释放，不产生写/独占请求。
            let mut rounds: u64 = 0;
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
                rounds += 1;
                // 等锁期间必须继续应答 TLB shootdown：本 hart 在 trap/syscall 里
                // 通常关中断，取不到 shootdown IPI；而锁持有者可能正卡在
                // `drain_shootdown` 里同步等本 hart 的 ack —— 双方互等，整机停摆
                // （观测：reqs 923 / acks 714，两个 hart 抱着各自任务的内核栈不
                // 动，其余核空闲）。`complete_current_pending_flush` 只做本核
                // invtlb + 原子发布，不取任何锁，可安全在自旋中调用。
                // 每 256 轮一次：无待办时仅一次 relaxed 读，热路径代价可忽略。
                if rounds & 0xff == 0 {
                    crate::mm::tlb::complete_current_pending_flush();
                }
                // SPINDBG：争用超过 ~400 万轮（≈几百 ms TCG）时把持有者与
                // 请求者 RA 打出来——死锁现场直接点名，不用 GDB 猜。
                if rounds >= (1 << 22) && !reported {
                    reported = true;
                    if crate::cmdline::spindbg_enabled() {
                        let (hra, hpid) = self.holder();
                        let my_pid = crate::sched::process_sched::HART_CURRENT_PID
                            .load(core::sync::atomic::Ordering::Relaxed);
                        crate::println!(
                            "[SPINDBG] lock={:#x} held_by_ra={:#x} held_by_pid={} waiter_ra={:#x} waiter_pid={} rounds={}",
                            self as *const Self as usize, hra, hpid, caller_ra, my_pid, rounds
                        );
                    }
                }
            }
            // 只在真的发生了争用时各做一次累加（快路径完全不碰这些计数器）。
            LOCK_CONTENDED.fetch_add(1, Ordering::Relaxed);
            LOCK_SPIN_ROUNDS.fetch_add(rounds, Ordering::Relaxed);
        }
    }
    
    /// 尝试获取锁（非阻塞）
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        if self.locked.compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ).is_ok() {
            Some(SpinLockGuard { lock: self })
        } else {
            None
        }
    }
    
    /// 检查是否被锁定
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

/// 关中断自旋锁：获取时先关本地中断，释放时恢复原状态。
///
/// 用于「事件投递路径可在任意任务上下文进入、且持锁期间绝不能被时钟抢占」
/// 的共享结构（如 inotify 实例队列）。普通 `SpinLock` 持锁者被时钟中断
/// 抢占后，若下一个任务在关中断路径里自旋等同一把锁，单核下持锁者永远
/// 得不到调度 → 整机死锁。关中断持锁从根上消除该窗口。
///
/// 约束：临界区内禁止任何可能睡眠/调度的操作。
pub struct IrqSpinLock<T> {
    locked: AtomicBool,
    /// 最近一次成功持锁者的调用点（lock() 的返回地址）与 pid，仅诊断用。
    holder_ra: core::sync::atomic::AtomicUsize,
    holder_pid: core::sync::atomic::AtomicUsize,
    data: UnsafeCell<T>,
}

pub struct IrqSpinLockGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    was_enabled: bool,
}

unsafe impl<T: Send> Sync for IrqSpinLock<T> {}
unsafe impl<T: Send> Send for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            holder_ra: core::sync::atomic::AtomicUsize::new(0),
            holder_pid: core::sync::atomic::AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let was_enabled = crate::arch::is_interrupt_enabled();
        crate::arch::disable_interrupts();
        // TTAS：与 `SpinLock::lock` 同款，抢不到就只读自旋，避免多核写乒乓。
        let mut spins: u64 = 0;
        loop {
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
                spins += 1;
                // 同 `SpinLock::lock`：中断已被本函数关掉，只能靠这里主动应答
                // 远端的 TLB shootdown，否则与持锁者的 drain 形成互等。
                if spins & 0xff == 0 {
                    crate::mm::tlb::complete_current_pending_flush();
                }
                if spins == 200_000_000 {
                    // 疑似锁泄漏。**不得用 println!**：卡住的锁很可能就是控制台
                    // 锁本身（它也是 IrqSpinLock），在等锁上下文里再 println 即
                    // 对同一把锁自递归，把"告警"变成二次死锁（实测整机冻结、
                    // [IRQLOCK] stuck 的 holder_ra==self_ra 即此形态）。走 panic
                    // 同款的无锁直写。
                    crate::console::raw_emergency_print(format_args!(
                        "[IRQLOCK] stuck: holder_ra={:#x} holder_pid={} self_ra={:#x}\n",
                        self.holder_ra.load(Ordering::Relaxed),
                        self.holder_pid.load(Ordering::Relaxed),
                        return_address(),
                    ));
                }
            }
        }
        self.holder_ra.store(return_address(), Ordering::Relaxed);
        self.holder_pid.store(current_pid_hint(), Ordering::Relaxed);
        IrqSpinLockGuard { lock: self, was_enabled }
    }
}

#[inline(always)]
fn return_address() -> usize {
    let ra: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, ra", out(reg) ra); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $ra", out(reg) ra); }
    ra
}

#[inline(always)]
fn current_pid_hint() -> usize {
    crate::sched::process_sched::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed)
}

impl<'a, T> Deref for IrqSpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> DerefMut for IrqSpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T> Drop for IrqSpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.was_enabled {
            crate::arch::enable_interrupts();
        }
    }
}

impl<'a, T> Deref for SpinLockGuard<'a, T> {
    type Target = T;
    
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> DerefMut for SpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T> Drop for SpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.holder_ra.store(0, Ordering::Relaxed);
        self.lock.holder_pid.store(0, Ordering::Relaxed);
        self.lock.locked.store(false, Ordering::Release);
    }
}
