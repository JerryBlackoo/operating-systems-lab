//! RustMicroOS 内核构建脚本
//!
//! 此脚本在编译内核时自动运行，负责：
//! 1. 根据目标架构选择对应的链接脚本
//! 2. 确保链接脚本变更时重新编译

use std::env;
use std::path::PathBuf;

fn main() {
    // 获取内核包的目录（build.rs 所在目录）
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let target = env::var("TARGET").unwrap_or_default();

    // 根据目标架构选择链接脚本
    // Select linker script based on target architecture
    let linker_script_name = if target.starts_with("loongarch64") {
        "linker-la.ld"
    } else {
        "linker.ld"
    };

    let linker_script = PathBuf::from(&manifest_dir)
        .join("src")
        .join(linker_script_name);

    // 设置内核链接脚本（使用绝对路径）
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());

    // 当链接脚本变更时，重新运行此构建脚本
    println!("cargo:rerun-if-changed=src/linker.ld");
    println!("cargo:rerun-if-changed=src/linker-la.ld");

    // 当构建脚本本身变更时，也重新运行
    println!("cargo:rerun-if-changed=build.rs");

    // 追踪内嵌的 init 二进制文件，确保 init 更新后 kernel 重编译
    let init_bin = if target.starts_with("loongarch64") {
        PathBuf::from(&manifest_dir)
            .join("../target/loongarch64-unknown-none/release/init")
    } else {
        PathBuf::from(&manifest_dir)
            .join("../target/riscv64gc-unknown-none-elf/release/init")
    };
    println!("cargo:rerun-if-changed={}", init_bin.display());
}
