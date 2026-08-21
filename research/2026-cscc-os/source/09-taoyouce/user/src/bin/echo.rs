//! echo - 输出文本
//!
//! 用法: echo [文本...]

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    for i in 1..argc {
        if i > 1 {
            print!(" ");
        }
        print!("{}", argv[i]);
    }
    println!();
    0
}
