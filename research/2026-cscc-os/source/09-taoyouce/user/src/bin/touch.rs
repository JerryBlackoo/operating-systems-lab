//! touch - 创建空文件或更新时间戳
//!
//! 用法: touch <文件名>

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::{open, close, OpenFlags};

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    if argc < 2 {
        println!("Usage: touch <filename>");
        return 1;
    }
    
    let path = argv[1];
    
    // 尝试创建或打开文件
    match open(path, OpenFlags::WRONLY | OpenFlags::CREATE) {
        Ok(fd) => {
            // 立即关闭，只是创建/更新时间戳
            let _ = close(fd);
            println!("Touched: {}", path);
            0
        }
        Err(e) => {
            println!("touch: cannot touch '{}': {:?}", path, e);
            1
        }
    }
}
