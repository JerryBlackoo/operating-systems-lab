//! 启动时从平台 RTC 初始化 CLOCK_REALTIME（对标 Linux `rtc_hctosys`）。
//!
//! 没有这一步，wallclock 从 1970 epoch 起算，而磁盘 rootfs 上的文件
//! mtime 是真实日期——`now < mtime` 的负时间差会让 cargo/rustup 等
//! 计算文件年龄时整数转换越界、构建缓存判定全部失真。
//!
//! - RISC-V `virt` 机型：Goldfish RTC @ 0x101000（TIME_LOW/TIME_HIGH，
//!   返回自 epoch 的纳秒数）。
//! - LoongArch `virt` 机型：LS7A RTC @ 0x100d0100（TOY 寄存器组，
//!   日历格式，年基于 1900、月为 1 基）。

/// 从平台 RTC 读取当前 wallclock 并设置 CLOCK_REALTIME 偏移。
/// 读数不可信（RTC 缺失/未使能/年份早于 2000）时保持 offset 不变。
pub fn init_wallclock() {
    if let Some(wall_nsec) = read_rtc_epoch_nsec() {
        // 1970..2000 之间视为 RTC 无效（QEMU 缺 RTC 设备时读 0）。
        const YEAR_2000_NSEC: i64 = 946_684_800 * 1_000_000_000;
        if wall_nsec >= YEAR_2000_NSEC {
            crate::syscall::time::init_wallclock_from_rtc(wall_nsec);
            crate::println!(
                "[rtc] wallclock set from RTC: {} s since epoch",
                wall_nsec / 1_000_000_000
            );
            return;
        }
    }
    crate::println!("[rtc] no usable RTC, wallclock starts at epoch");
}

#[cfg(target_arch = "riscv64")]
fn read_rtc_epoch_nsec() -> Option<i64> {
    // Goldfish RTC：读 TIME_LOW 会锁存 TIME_HIGH，须先读 LOW。
    const GOLDFISH_RTC_BASE: usize = 0x10_1000;
    const TIME_LOW: usize = 0x00;
    const TIME_HIGH: usize = 0x04;
    let low = unsafe {
        core::ptr::read_volatile((GOLDFISH_RTC_BASE + TIME_LOW) as *const u32)
    } as u64;
    let high = unsafe {
        core::ptr::read_volatile((GOLDFISH_RTC_BASE + TIME_HIGH) as *const u32)
    } as u64;
    let nsec = (high << 32) | low;
    i64::try_from(nsec).ok()
}

#[cfg(target_arch = "loongarch64")]
fn read_rtc_epoch_nsec() -> Option<i64> {
    // LS7A RTC 经 DMW1（非缓存窗口）访问；TOY 读数为日历时间。
    const LS7A_RTC_BASE: usize = 0x8000_0000_100d_0100;
    const SYS_TOYREAD0: usize = 0x2c;
    const SYS_TOYREAD1: usize = 0x30;
    const SYS_RTCCTRL: usize = 0x40;
    const TOY_ENABLE: u32 = 1 << 11;
    const OSC_ENABLE: u32 = 1 << 8;

    unsafe {
        // TOY 计时依赖 TOY_ENABLE|OSC_ENABLE（Linux rtc-loongson 同样先使能）。
        let ctrl_ptr = (LS7A_RTC_BASE + SYS_RTCCTRL) as *mut u32;
        let ctrl = core::ptr::read_volatile(ctrl_ptr);
        if ctrl & (TOY_ENABLE | OSC_ENABLE) != (TOY_ENABLE | OSC_ENABLE) {
            core::ptr::write_volatile(ctrl_ptr, ctrl | TOY_ENABLE | OSC_ENABLE);
        }
        let val = core::ptr::read_volatile(
            (LS7A_RTC_BASE + SYS_TOYREAD0) as *const u32,
        );
        let year_reg = core::ptr::read_volatile(
            (LS7A_RTC_BASE + SYS_TOYREAD1) as *const u32,
        );
        // TOYREAD0 位域（与 Linux drivers/rtc/rtc-loongson.c 一致）：
        // [31:26]=月(1基) [25:21]=日 [20:16]=时 [15:10]=分 [9:4]=秒 [3:0]=0.1秒
        let mon = ((val >> 26) & 0x3f) as i64; // 1..=12
        let day = ((val >> 21) & 0x1f) as i64;
        let hour = ((val >> 16) & 0x1f) as i64;
        let min = ((val >> 10) & 0x3f) as i64;
        let sec = ((val >> 4) & 0x3f) as i64;
        let year = year_reg as i64 + 1900;
        if !(1..=12).contains(&mon) || !(1..=31).contains(&day) {
            return None;
        }
        let secs = days_from_civil(year, mon, day) * 86_400
            + hour * 3600
            + min * 60
            + sec;
        secs.checked_mul(1_000_000_000)
    }
}

/// 公历 (y,m,d) → 自 1970-01-01 的天数（Howard Hinnant days_from_civil）。
#[cfg(target_arch = "loongarch64")]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
