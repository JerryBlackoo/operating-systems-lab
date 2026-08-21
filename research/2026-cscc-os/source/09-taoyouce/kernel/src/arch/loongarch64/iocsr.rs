//! LoongArch IOCSR access helpers.

pub const IPI_STATUS: u32 = 0x1000;
pub const IPI_EN: u32 = 0x1004;
pub const IPI_CLEAR: u32 = 0x100c;
pub const MBUF_SEND: u32 = 0x1048;
pub const IPI_SEND: u32 = 0x1040;

pub const MBUF_SEND_BOX_LO: u32 = 0;
pub const MBUF_SEND_BOX_HI: u32 = 1;
pub const MBUF_SEND_BLOCKING: u64 = 1 << 31;
pub const MBUF_SEND_BOX_SHIFT: u32 = 2;
pub const MBUF_SEND_CPU_SHIFT: u32 = 16;
pub const MBUF_SEND_BUF_SHIFT: u32 = 32;

pub const IPI_SEND_BLOCKING: u32 = 1 << 31;
pub const IPI_SEND_CPU_SHIFT: u32 = 16;

pub const ACTION_BOOT_CPU: u32 = 1 << 0;
pub const ACTION_RESCHEDULE: u32 = 1 << 1;
pub const ACTION_TLB_SHOOTDOWN: u32 = 1 << 2;
pub const ACTION_FUNCTION_CALL: u32 = 1 << 3;
pub const ACTION_STOP: u32 = 1 << 4;

/// 按 Linux `ipi_write_action` 语义向 `cpu` 投递 IPI：`IPI_SEND[4:0]` 是
/// **位索引**而非位掩码，把 action 掩码拆成逐位 index 写入。
///
/// 直接写掩码会置错位：shootdown 掩码 0x4 落到对端 status 的 bit4（STOP 位），
/// 接收端按掩码匹配不到，IPI 被 `IPI_CLEAR` 静默丢弃——LA BuildStorm 实测
/// `[tlbstat] shootdown reqs=1036 acks=2 timeouts=1264` 的根因；reschedule
/// 掩码 0x2 则落到 bit2 被误读成 shootdown（那 2 次假 ack 的来源）。
pub unsafe fn send_ipi_action(cpu: usize, action: u32) {
    let mut rest = action;
    while rest != 0 {
        let bit = rest.trailing_zeros();
        let value = IPI_SEND_BLOCKING | ((cpu as u32) << IPI_SEND_CPU_SHIFT) | bit;
        write32(IPI_SEND, value);
        rest &= !(1u32 << bit);
    }
}

#[inline]
pub unsafe fn read8(addr: u32) -> u8 {
    let value: u8;
    core::arch::asm!("iocsrrd.b {}, {}", out(reg) value, in(reg) addr);
    value
}

#[inline]
pub unsafe fn read32(addr: u32) -> u32 {
    let value: u32;
    core::arch::asm!("iocsrrd.w {}, {}", out(reg) value, in(reg) addr);
    value
}

#[inline]
pub unsafe fn read64(addr: u32) -> u64 {
    let value: u64;
    core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) addr);
    value
}

#[inline]
pub unsafe fn write8(addr: u32, value: u8) {
    core::arch::asm!("iocsrwr.b {}, {}", in(reg) value, in(reg) addr);
}

#[inline]
pub unsafe fn write32(addr: u32, value: u32) {
    core::arch::asm!("iocsrwr.w {}, {}", in(reg) value, in(reg) addr);
}

#[inline]
pub unsafe fn write64(addr: u32, value: u64) {
    core::arch::asm!("iocsrwr.d {}, {}", in(reg) value, in(reg) addr);
}
