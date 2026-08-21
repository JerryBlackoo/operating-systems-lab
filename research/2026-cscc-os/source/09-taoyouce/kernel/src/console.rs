//! 控制台输出
//! 
//! 提供 UART 串口输出和 println! 宏

#![allow(dead_code)]

use core::fmt::{self, Write};

/// UART 基地址
/// RISC-V64 QEMU virt: 0x1000_0000 (ns16550a)
/// LoongArch64 QEMU virt: 0x1FE0_01E0 (16550 兼容)
#[cfg(all(feature = "qemu", target_arch = "riscv64"))]
const UART_BASE: usize = 0x1000_0000;

#[cfg(all(feature = "qemu", target_arch = "loongarch64"))]
const UART_BASE: usize = 0x8000_0000_1FE0_01E0; // DMW1 窗口: 0x8000_0000_0000_0000 | 物理地址

#[cfg(feature = "k210")]
const UART_BASE: usize = 0x3800_0000;

#[cfg(not(any(feature = "qemu", feature = "k210")))]
const UART_BASE: usize = 0x1000_0000;

/// UART 寄存器偏移
const UART_THR: usize = 0; // 发送保持寄存器
const UART_LSR: usize = 5; // 线路状态寄存器
const UART_LSR_TX_EMPTY: u8 = 0x20;

/// UART 驱动
pub struct Uart {
    base: usize,
}

impl Uart {
    /// 创建 UART 实例
    pub const fn new(base: usize) -> Self {
        Self { base }
    }
    
    /// 初始化 UART
    pub fn init(&self) {
        // QEMU virt 平台的 UART 已经初始化好了
        // K210 需要额外配置
        #[cfg(feature = "k210")]
        {
            // K210 UART 初始化代码
            // TODO: 实现 K210 UART 初始化
        }
    }
    
    /// 发送一个字节
    pub fn putc(&self, c: u8) {
        unsafe {
            let thr = (self.base + UART_THR) as *mut u8;
            let lsr = (self.base + UART_LSR) as *const u8;
            
            // 等待发送缓冲区空
            while lsr.read_volatile() & UART_LSR_TX_EMPTY == 0 {}
            
            // 发送字符
            thr.write_volatile(c);
        }
    }
    
    /// 接收一个字节（非阻塞）
    pub fn getc(&self) -> Option<u8> {
        unsafe {
            let lsr = (self.base + UART_LSR) as *const u8;
            
            // 检查是否有数据
            if lsr.read_volatile() & 0x01 != 0 {
                let rbr = self.base as *const u8;
                Some(rbr.read_volatile())
            } else {
                None
            }
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.bytes() {
            if c == b'\n' {
                self.putc(b'\r');
            }
            self.putc(c);
        }
        Ok(())
    }
}

/// 全局 UART 实例。
///
/// 用**关中断**自旋锁（对标 Linux printk 在关中断下操作 console/logbuf）：
/// 纯 `spin::Mutex` 下，持锁写串口的 hart 一旦被本地中断打断，而中断处理里
/// 又执行任意 `println!`，就会在同一 hart 上自死锁。AP 真正参与执行后，内核态
/// 也会收到定时器中断（idle 的 wfi 窗口开中断），该窗口是真实存在的：观测到
/// 启动期串口停在 `[kernel] Hart 0` 打印中途，且此前紧跟着定时器中断路径的
/// 打印。关中断持锁把这个窗口从根上消除；跨 hart 仍按自旋互斥排队，保证
/// 每条 `println!` 输出不被其它 hart 穿插（官方判分要 grep 完整行）。
static UART: crate::sync::IrqSpinLock<Uart> = crate::sync::IrqSpinLock::new(Uart::new(UART_BASE));

/// 初始化控制台
pub fn init() {
    UART.lock().init();
}

/// panic/oops 期间绕过 UART 锁的开关。
///
/// 对标 Linux `bust_spinlocks(1)`（`oops_enter` 会调用）：一旦进入 panic 路径，
/// 控制台不能再依赖锁——持锁者可能正是那个已经崩掉/halt 的 CPU，其它 CPU 只要
/// 想打印就会永久自旋，把真正的错误信息全部吞掉。
///
/// 实测（SMP 使能期）：串口停在 `[trap] KPF hart=` 打印中途，整机再无输出，
/// 根因信息完全不可见。置位后 `_print` 直接写 UART，宁可极少数情况下输出交错，
/// 也必须保证崩溃现场能打完。
static PANIC_BUST: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 进入 panic/致命错误路径时调用：此后控制台不再加锁。
pub fn bust_locks() {
    PANIC_BUST.store(true, core::sync::atomic::Ordering::Release);
}

/// 无锁紧急输出：绕开 UART 锁直写。
///
/// 仅供「可能正卡在控制台锁上」的告警路径使用（如 IrqSpinLock 自旋超限）——
/// 那种上下文里再走 `_print` 抢锁等于对同一把锁自递归。输出可能与其它 hart
/// 交错，紧急路径接受这个代价。
pub fn raw_emergency_print(args: fmt::Arguments) {
    let mut uart = Uart::new(UART_BASE);
    let _ = uart.write_fmt(args);
}

/// 打印函数
pub fn _print(args: fmt::Arguments) {
    if PANIC_BUST.load(core::sync::atomic::Ordering::Acquire) {
        // 无锁直写：崩溃现场优先保证可见性（Linux oops 同款取舍）。
        let mut uart = Uart::new(UART_BASE);
        let _ = uart.write_fmt(args);
        return;
    }
    UART.lock().write_fmt(args).unwrap();
}

/// 从控制台非阻塞读取一个字节（供 stdin fd 0 使用）。
///
/// 复用与 `_print` 相同、按架构正确配置的 `UART_BASE`（RISC-V `0x1000_0000`、
/// LoongArch DMW1 窗口 `0x8000_0000_1FE0_01E0`）。**绝不**在此硬编码 RISC-V 的
/// MMIO 地址：在 LoongArch 上读 `0x1000_0005` 是一个无 TLB/DMW 映射的地址，会
/// 触发内核 TLB refill 异常并 panic（crash02 模糊 `pread64(fd=0,…)` 命中 stdin
/// 读路径即可整核 halt）。
pub fn getc() -> Option<u8> {
    UART.lock().getc()
}

/// 调试打印接收端：仅在 `debug` 特性下真正输出。
///
/// 判题/release 构建（`--features qemu`，无 `debug`）下为空实现，
/// 内核侧的 `[tag]` 调试日志（例如 `[af_alg]` 在 socket 循环里的刷屏）
/// 既不占串口带宽也不耗写 UART 的 wall-clock。
/// 用 `--features debug` 重新打开全部调试输出。
#[cfg(feature = "debug")]
#[inline]
pub fn _dprint(args: fmt::Arguments) {
    UART.lock().write_fmt(args).unwrap();
}

/// 见上：release 构建下的空实现（参数仍被求值，避免 unused 警告）。
#[cfg(not(feature = "debug"))]
#[inline(always)]
pub fn _dprint(_args: fmt::Arguments) {}

/// println! 宏
#[macro_export]
macro_rules! println {
    () => ($crate::console::_print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::console::_print(format_args!("{}\n", format_args!($($arg)*))));
}

/// print! 宏
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

/// 调试 println! 宏：等价 println!，但只在 `debug` 特性下真正输出。
/// 用于内核侧 `[tag]` 调试日志，判题/release 构建里为空操作（不占串口）。
#[macro_export]
macro_rules! dprintln {
    () => ($crate::console::_dprint(format_args!("\n")));
    ($($arg:tt)*) => ($crate::console::_dprint(format_args!("{}\n", format_args!($($arg)*))));
}

/// 调试 print! 宏：见 `dprintln!`。
#[macro_export]
macro_rules! dprint {
    ($($arg:tt)*) => ($crate::console::_dprint(format_args!($($arg)*)));
}
