//! pwd - 显示当前工作目录
//!
//! 用法: pwd

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::getcwd;

#[no_mangle]
pub fn main(_argc: usize, _argv: &[&str]) -> i32 {
    match getcwd() {
        Ok(buf) => {
            // 找到字符串结尾
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            if let Ok(path) = core::str::from_utf8(&buf[..len]) {
                println!("{}", path);
                0
            } else {
                println!("pwd: invalid path encoding");
                1
            }
        }
        Err(e) => {
            println!("pwd: cannot get current directory: {:?}", e);
            1
        }
    }
}
