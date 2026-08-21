//! RustMicroOS 用户程序构建脚本
//!
//! 此脚本在编译用户程序时自动运行，负责：
//! 1. 设置用户程序专用的链接脚本
//! 2. 确保链接脚本变更时重新编译

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 获取用户程序包的目录（build.rs 所在目录）
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let linker_script = PathBuf::from(&manifest_dir).join("src").join("linker.ld");

    // 设置用户程序链接脚本（使用绝对路径）
    // 这样可以在 workspace 级别编译时正确找到链接脚本
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());

    // 当链接脚本变更时，重新运行此构建脚本
    println!("cargo:rerun-if-changed=src/linker.ld");
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");

    // 当构建脚本本身变更时，也重新运行
    println!("cargo:rerun-if-changed=build.rs");

    build_schedfix(&manifest_dir);
    build_hello(&manifest_dir);
}

fn build_schedfix(manifest_dir: &str) {
    let target_arch = match env::var("CARGO_CFG_TARGET_ARCH") {
        Ok(v) => v,
        Err(_) => return,
    };
    let compiler = match target_arch.as_str() {
        "riscv64" => "riscv64-buildroot-linux-musl-gcc",
        "loongarch64" => "loongarch64-linux-musl-gcc",
        _ => return,
    };

    let src = PathBuf::from(manifest_dir).join("src").join("libschedfix.c");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out = out_dir.join("libschedfix.so");
    println!("cargo:rerun-if-changed={}", src.display());

    let status = Command::new(compiler)
        .args([
            "-shared",
            "-fPIC",
            "-O2",
            "-Wl,-soname,libschedfix.so",
            "-o",
            out.to_str().unwrap(),
            src.to_str().unwrap(),
        ])
        .status()
        .expect("failed to invoke cross compiler for libschedfix.so");
    assert!(status.success(), "failed to build libschedfix.so");

    let meta = fs::metadata(&out).expect("libschedfix.so missing after build");
    assert!(meta.len() > 0, "libschedfix.so is empty");
    println!("cargo:rustc-env=RUSTOS_SCHEDFIX_SO={}", out.display());
}

/// Cross-compile a minimal static ELF (`hello.c`) used as the target of
/// lmbench `lat_proc` exec/shell benchmarks, replacing the broken wrapper that
/// ships in the test image. Same mechanism as `build_schedfix`.
fn build_hello(manifest_dir: &str) {
    let target_arch = match env::var("CARGO_CFG_TARGET_ARCH") {
        Ok(v) => v,
        Err(_) => return,
    };
    let compiler = match target_arch.as_str() {
        "riscv64" => "riscv64-buildroot-linux-musl-gcc",
        "loongarch64" => "loongarch64-linux-musl-gcc",
        _ => return,
    };

    let src = PathBuf::from(manifest_dir).join("src").join("hello.c");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out = out_dir.join("hello");
    println!("cargo:rerun-if-changed={}", src.display());

    let status = Command::new(compiler)
        .args([
            "-static",
            "-O2",
            "-o",
            out.to_str().unwrap(),
            src.to_str().unwrap(),
        ])
        .status()
        .expect("failed to invoke cross compiler for hello");
    assert!(status.success(), "failed to build hello");

    let meta = fs::metadata(&out).expect("hello missing after build");
    assert!(meta.len() > 0, "hello is empty");
    println!("cargo:rustc-env=RUSTOS_HELLO_ELF={}", out.display());
}
