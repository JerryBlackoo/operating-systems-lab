#[cfg(target_arch = "loongarch64")]
use devices::VIRT_ADDR_START;

#[cfg(target_arch = "loongarch64")]
pub fn shutdown() -> ! {
    let ged_addr = (0x100e001c | VIRT_ADDR_START) as *mut u8;
    log::info!("Shutting down...");
    unsafe {
        ged_addr.write_volatile(0x34);
    }
    loop {
        unsafe {
            core::arch::asm!("idle 1");
        }
    }
}

#[cfg(not(target_arch = "loongarch64"))]
pub fn shutdown() -> ! {
    polyhal::instruction::shutdown()
}
