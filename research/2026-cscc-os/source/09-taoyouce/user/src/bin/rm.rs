//! rm - 删除文件或目录
//!
//! 用法: rm [-r] <文件或目录>
//! 选项:
//!   -r  递归删除目录

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::{unlink, rmdir, FsError};

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    if argc < 2 {
        println!("Usage: rm [-r] <file or directory>");
        return 1;
    }
    
    let mut recursive = false;
    let mut path_idx = 1;
    
    // 解析选项
    if argc >= 2 && argv[1] == "-r" {
        recursive = true;
        path_idx = 2;
    }
    
    if path_idx >= argc {
        println!("rm: missing operand");
        return 1;
    }
    
    let path = argv[path_idx];
    
    // 尝试删除文件
    match unlink(path) {
        Ok(()) => {
            println!("Removed: {}", path);
            return 0;
        }
        Err(FsError::IsDirectory) => {
            // 是目录，检查是否有 -r 选项
            if !recursive {
                println!("rm: cannot remove '{}': Is a directory (use -r)", path);
                return 1;
            }
        }
        Err(e) => {
            println!("rm: cannot remove '{}': {:?}", path, e);
            return 1;
        }
    }
    
    // 递归删除目录
    match rmdir(path) {
        Ok(()) => {
            println!("Removed directory: {}", path);
            0
        }
        Err(FsError::NotEmpty) => {
            println!("rm: cannot remove '{}': Directory not empty", path);
            println!("Note: Recursive deletion of non-empty directories not yet implemented");
            1
        }
        Err(e) => {
            println!("rm: cannot remove '{}': {:?}", path, e);
            1
        }
    }
}
