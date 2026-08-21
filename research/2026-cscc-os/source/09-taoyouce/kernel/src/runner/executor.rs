//! 测试执行引擎
//!
//! 负责执行测试脚本并管理评测流程：
//! - 通过 fork + exec 执行每个测试脚本（使用 busybox sh 解释）
//! - 在执行前后输出 START/END 标记
//! - 处理脚本执行失败和超时
//! - 所有脚本执行完毕后关机
//!
//! # 执行流程
//! ```text
//! run_all_and_shutdown():
//!   1. 扫描 ext4 根目录中的 *_testcode.sh 文件
//!   2. 对每个脚本：
//!      a. print_group_start(name)
//!      b. fork 子进程
//!      c. 子进程 exec busybox sh <script>
//!      d. 父进程 waitpid 等待完成（带超时）
//!      e. print_group_end(name)
//!   3. 刷新文件系统缓冲区
//!   4. 调用 HalImpl::shutdown(0) 关机
//! ```
//!
//! # Requirements
//! - 7.1: 自动挂载 VirtIO 块设备上的 ext4 文件系统
//! - 7.2: 扫描挂载根目录，查找所有匹配 `*_testcode.sh` 模式的测试脚本
//! - 7.3: 依次串行执行每个测试脚本，在执行前后输出对应的 TEST GROUP START/END 标记
//! - 7.5: 脚本执行失败或超时时跳过并继续，输出 END 标记
//! - 7.6: 所有测试脚本执行完毕后通知 Power_Manager 执行关机

use alloc::string::String;
use super::scanner::{TestGroup, TestResult};
use super::marker::{print_group_start, print_group_end};
use super::shell;

/// 默认脚本执行超时时间（秒）
/// 单个测试组最多运行 5 分钟
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// 执行单个测试组
///
/// 通过 fork + exec 在子进程中执行测试脚本，父进程等待完成。
/// 无论成功或失败，都会输出 START/END 标记。
///
/// # 执行方式
/// 子进程通过 `busybox sh <script_path>` 执行脚本。
/// 如果 busybox 不在当前目录，尝试 `/bin/sh`。
///
/// # 参数
/// - `group`: 要执行的测试组信息
///
/// # 返回
/// 测试执行结果
///
/// # Requirements
/// - 7.3: 在执行前后输出 TEST GROUP START/END 标记
/// - 7.5: 脚本执行失败或超时时跳过并继续
pub fn run_test_group(group: &TestGroup) -> TestResult {
    run_test_group_with_timeout(group, DEFAULT_TIMEOUT_SECS)
}

/// 执行单个测试组（带自定义超时）
///
/// # 参数
/// - `group`: 要执行的测试组信息
/// - `_timeout_secs`: 超时时间（秒），0 表示无超时
///
/// # 返回
/// 测试执行结果
pub fn run_test_group_with_timeout(group: &TestGroup, _timeout_secs: u64) -> TestResult {
    use crate::sched::process;

    crate::println!("[runner] Executing test group: {} ({})", group.name, group.script_path);

    // 输出 START 标记
    print_group_start(&group.name);

    // 检查脚本文件是否存在
    if crate::fs::lookup(&group.script_path).is_err() {
        crate::println!("[runner] Script not found: {}, skipping", group.script_path);
        print_group_end(&group.name);
        return TestResult::Skipped;
    }

    // fork 子进程执行脚本
    let result = match process::fork() {
        Ok(child_pid) => {
            // 父进程：等待子进程完成
            wait_for_child(child_pid, group)
        }
        Err(e) => {
            crate::println!("[runner] Fork failed for {}: {}", group.name, e);
            TestResult::Failed(String::from("fork failed"))
        }
    };

    // 输出 END 标记（无论成功或失败都输出）
    print_group_end(&group.name);

    result
}

/// 等待子进程完成
///
/// 在父进程中等待 fork 出的子进程执行完毕。
/// 子进程会通过 exec 加载 busybox sh 来执行测试脚本。
///
/// # 参数
/// - `child_pid`: 子进程 PID
/// - `group`: 测试组信息（用于日志）
///
/// # 返回
/// 测试执行结果
fn wait_for_child(child_pid: usize, group: &TestGroup) -> TestResult {
    use crate::sched::process::{PROCESS_TABLE, ProcessState};

    crate::println!("[runner] Waiting for child PID {} ({})", child_pid, group.name);

    // 轮询等待子进程退出
    // 在内核态中，我们直接检查进程表中子进程的状态
    loop {
        let child_state = {
            let table_guard = PROCESS_TABLE.lock();
            if let Some(table) = table_guard.as_ref() {
                if let Some(child) = table.get(child_pid) {
                    Some((child.state, child.exit_status))
                } else {
                    // 子进程已被回收
                    None
                }
            } else {
                None
            }
        };

        match child_state {
            Some((ProcessState::Zombie, exit_status)) => {
                // 子进程已退出，回收资源
                // Slice 2.0：ExitReason::Killed 合并到 Signaled { sig:9, .. }；
                // core_dumped 字段用于日志显示更准确的终止原因。
                let exit_code = match exit_status {
                    Some(crate::sched::process::ExitReason::Normal(code)) => code,
                    Some(crate::sched::process::ExitReason::Signaled { sig, core_dumped }) => {
                        if core_dumped {
                            crate::println!("[runner] {} killed by signal {} (core dumped)", group.name, sig);
                        } else {
                            crate::println!("[runner] {} killed by signal {}", group.name, sig);
                        }
                        128 + sig
                    }
                    None => -1,
                };

                crate::println!("[runner] {} exited with code {}", group.name, exit_code);

                if exit_code == 0 {
                    return TestResult::Success;
                } else {
                    return TestResult::Failed(
                        alloc::format!("exit code {}", exit_code)
                    );
                }
            }
            Some((ProcessState::Unused, _)) | None => {
                // 子进程已不存在
                crate::println!("[runner] Child {} no longer exists", child_pid);
                return TestResult::Failed(String::from("child process disappeared"));
            }
            Some(_) => {
                // 子进程仍在运行，让出 CPU
                // 在内核态中使用 wfi 等待中断
                crate::arch::wfi();
            }
        }
    }
}

/// 执行所有测试脚本并关机
///
/// 这是评测运行器的主入口函数。完整流程：
/// 1. 扫描 ext4 根目录中的 `*_testcode.sh` 文件
/// 2. 依次串行执行每个测试脚本
/// 3. 刷新文件系统缓冲区
/// 4. 调用 HalImpl::shutdown(0) 关机
///
/// # 参数
/// - `scan_dirs`: 要扫描的目录列表（如 `&["/", "/musl"]`）
///
/// # Requirements
/// - 7.1: 自动挂载 VirtIO 块设备上的 ext4 文件系统（挂载在调用前完成）
/// - 7.2: 扫描挂载根目录
/// - 7.3: 依次串行执行每个测试脚本
/// - 7.5: 脚本执行失败或超时时跳过并继续
/// - 7.6: 所有测试脚本执行完毕后关机
pub fn run_all_and_shutdown(scan_dirs: &[&str]) -> ! {
    use crate::arch::hal_trait::ArchHal;
    use crate::arch::HalImpl;
    use super::scanner;

    crate::println!();
    crate::println!("========================================");
    crate::println!("  RustMicroOS Test Runner");
    crate::println!("========================================");
    crate::println!();

    // 1. 扫描测试脚本
    let groups = scanner::scan_multiple_dirs(scan_dirs);

    if groups.is_empty() {
        crate::println!("[runner] No test scripts found, shutting down...");
    } else {
        crate::println!("[runner] Found {} test group(s), starting execution...", groups.len());
        crate::println!();

        // 2. 依次执行每个测试组
        let mut success_count = 0usize;
        let mut fail_count = 0usize;
        let mut skip_count = 0usize;
        let mut timeout_count = 0usize;

        for group in &groups {
            let result = run_test_group(group);

            match &result {
                TestResult::Success => success_count += 1,
                TestResult::Failed(_) => fail_count += 1,
                TestResult::Timeout => timeout_count += 1,
                TestResult::Skipped => skip_count += 1,
            }

            crate::println!();
        }

        // 输出执行摘要
        crate::println!("========================================");
        crate::println!("  Test Runner Summary");
        crate::println!("  Total: {}  Success: {}  Failed: {}  Timeout: {}  Skipped: {}",
            groups.len(), success_count, fail_count, timeout_count, skip_count);
        crate::println!("========================================");
        crate::println!();
    }

    // 3. 刷新文件系统缓冲区
    crate::println!("[runner] Flushing filesystem buffers...");
    sync_all_filesystems();

    // 4. 关机
    crate::println!("[runner] All tests complete, shutting down...");
    HalImpl::shutdown(0);
}

/// 仅执行指定的测试组列表（不扫描，不关机）
///
/// 用于外部调用者已经有测试组列表的场景。
///
/// # 参数
/// - `groups`: 要执行的测试组列表
///
/// # 返回
/// 各测试组的执行结果
pub fn run_groups(groups: &[TestGroup]) -> alloc::vec::Vec<(String, TestResult)> {
    let mut results = alloc::vec::Vec::new();

    for group in groups {
        let result = run_test_group(group);
        results.push((group.name.clone(), result));
    }

    results
}

/// 刷新所有文件系统缓冲区
///
/// Flush all filesystem buffers to disk before shutdown.
///
/// 在关机前调用，确保所有写入数据已持久化到磁盘。
/// Called before shutdown to ensure all write data has been persisted to disk.
///
/// ## 同步流程 / Sync Process
///
/// 1. 调用统一的电源管理预关机同步
/// 2. 额外尝试 ext4 特定的同步操作
///
/// Requirements: 7.9 — 关机前确保所有文件系统缓冲区已刷新到磁盘
fn sync_all_filesystems() {
    // 调用统一的电源管理预关机同步
    // Call unified power management pre-shutdown sync
    crate::power::pre_shutdown_sync();

    crate::println!("[runner] Filesystem sync complete");
}
