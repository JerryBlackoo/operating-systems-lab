//! 用户态控制台输出

use core::fmt::{self, Write};
use crate::syscall::sys_write;

/// 标准输出文件描述符
const STDOUT: usize = 1;

struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys_write(STDOUT, s.as_bytes());
        Ok(())
    }
}

/// 打印函数
pub fn print(args: fmt::Arguments) {
    Stdout.write_fmt(args).unwrap();
}

/// print! 宏
#[macro_export]
macro_rules! print {
    () => {};
    ($fmt: literal $(, $($arg: tt)+)?) => {
        $crate::console::print(format_args!($fmt $(, $($arg)+)?));
    }
}

/// println! 宏
#[macro_export]
macro_rules! println {
    () => {
        $crate::console::print(format_args!("\n"));
    };
    ($fmt: literal $(, $($arg: tt)+)?) => {
        $crate::console::print(format_args!(concat!($fmt, "\n") $(, $($arg)+)?));
    }
}
