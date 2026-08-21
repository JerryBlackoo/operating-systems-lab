//! cat - 显示文件内容
//!
//! 用法: cat <文件>

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::{open, read, close, OpenFlags};

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    if argc < 2 {
        println!("Usage: cat <file>");
        return 1;
    }
    
    let path = argv[1];
    
    let fd = match open(path, OpenFlags::RDONLY) {
        Ok(fd) => fd,
        Err(e) => {
            println!("cat: {}: {:?}", path, e);
            return 1;
        }
    };
    
    let mut buf = [0u8; 512];
    loop {
        match read(fd, &mut buf) {
            Ok(0) => break,  // EOF
            Ok(n) => {
                if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                    print!("{}", s);
                } else {
                    // 非 UTF-8 数据，按十六进制输出
                    for byte in &buf[..n] {
                        print!("{:02x} ", byte);
                    }
                }
            }
            Err(e) => {
                println!("\ncat: read error: {:?}", e);
                let _ = close(fd);
                return 1;
            }
        }
    }
    
    let _ = close(fd);
    0
}
