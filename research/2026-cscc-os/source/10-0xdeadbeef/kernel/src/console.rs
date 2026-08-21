use core::fmt;

use myos_runtime::console::ByteConsole;

use crate::irq_lock::IrqSpinLock;
use crate::lockdep::{LockClass, LockRank};

const CONSOLE_WRITE_CLASS: LockClass = LockClass::new("console.write", LockRank::Console, 3);
static CONSOLE_WRITE_LOCK: IrqSpinLock<()> = IrqSpinLock::new_with_class((), CONSOLE_WRITE_CLASS); // SUDOOS_FINAL_DIRECT_FIX_V1

/// 将当前架构的早期控制台适配到公共格式化设施。
struct EarlyConsole;

impl ByteConsole for EarlyConsole {
    #[inline]
    fn write_byte(byte: u8) {
        crate::arch::early_console::write_byte(byte);
    }
}

#[doc(hidden)]
pub fn print(arguments: fmt::Arguments<'_>) {
    let _guard = CONSOLE_WRITE_LOCK.lock();
    myos_runtime::console::write::<EarlyConsole>(arguments);
}

/// Serialize one real userspace write with kernel diagnostics on SMP.
pub fn write_bytes(bytes: &[u8]) {
    let _guard = CONSOLE_WRITE_LOCK.lock();
    for byte in bytes {
        crate::arch::early_console::write_byte(*byte);
    }
}

#[macro_export]
macro_rules! print {
    ($($argument:tt)*) => {
        $crate::console::print(format_args!($($argument)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };

    ($($argument:tt)*) => {
        $crate::print!("{}\n", format_args!($($argument)*))
    };
}
