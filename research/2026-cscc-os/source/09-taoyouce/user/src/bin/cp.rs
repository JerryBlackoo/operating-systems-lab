//! cp - 复制文件
//!
//! 用法: cp <源文件> <目标文件>

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::copy_file;

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    if argc < 3 {
        println!("Usage: cp <source> <destination>");
        return 1;
    }
    
    let src = argv[1];
    let dst = argv[2];
    
    match copy_file(src, dst) {
        Ok(bytes) => {
            println!("Copied {} -> {} ({} bytes)", src, dst, bytes);
            0
        }
        Err(e) => {
            println!("cp: cannot copy '{}' to '{}': {:?}", src, dst, e);
            1
        }
    }
}
