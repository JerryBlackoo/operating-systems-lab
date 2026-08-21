//! ls - 列出目录内容
//!
//! 用法: ls [目录]

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::{open, readdir, close, OpenFlags, DirEntry, FileType};

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    let path = if argc > 1 { argv[1] } else { "." };
    
    let fd = match open(path, OpenFlags::RDONLY) {
        Ok(fd) => fd,
        Err(e) => {
            println!("ls: cannot access '{}': {:?}", path, e);
            return 1;
        }
    };
    
    println!("Directory: {}", path);
    println!("{:<20} {:>10} {:<10}", "Name", "Size", "Type");
    println!("------------------------------------------");
    
    let mut entries = [DirEntry {
        name: [0u8; 64],
        name_len: 0,
        file_type: FileType::Unknown,
        size: 0,
        inode: 0,
    }; 16];
    
    loop {
        match readdir(fd, &mut entries) {
            Ok(0) => break,  // 没有更多条目
            Ok(n) => {
                for entry in &entries[..n] {
                    let type_str = match entry.file_type {
                        FileType::File => "file",
                        FileType::Directory => "dir",
                        FileType::Symlink => "link",
                        FileType::Device => "dev",
                        FileType::Unknown => "???",
                    };
                    
                    println!("{:<20} {:>10} {:<10}", entry.name_str(), entry.size, type_str);
                }
            }
            Err(e) => {
                println!("ls: read error: {:?}", e);
                break;
            }
        }
    }
    
    let _ = close(fd);
    0
}
