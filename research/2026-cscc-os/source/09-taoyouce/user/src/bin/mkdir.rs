//! mkdir - 创建目录
//!
//! 用法: mkdir <目录名>

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::{mkdir, MkdirFlags};

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    if argc < 2 {
        println!("Usage: mkdir <directory>");
        return 1;
    }
    
    let path = argv[1];
    
    match mkdir(path, MkdirFlags::NONE) {
        Ok(()) => {
            println!("Created directory: {}", path);
            0
        }
        Err(e) => {
            println!("mkdir: cannot create directory '{}': {:?}", path, e);
            1
        }
    }
}
