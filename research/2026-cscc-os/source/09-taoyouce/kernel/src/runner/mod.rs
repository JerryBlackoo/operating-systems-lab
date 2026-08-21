//! 评测运行器模块
//!
//! 负责自动化评测流程：
//! - 输出评测标记（START/END）供官方评分脚本解析
//! - 扫描磁盘中的测试脚本（`*_testcode.sh`）
//! - 依次执行测试脚本并输出结果
//! - 执行完毕后关机
//!
//! # 模块结构
//! - `marker`: 评测标记输出（START/END 格式）
//! - `scanner`: 测试脚本扫描器（匹配 `*_testcode.sh`）
//! - `executor`: 测试执行引擎（fork + exec + 超时处理 + 关机）
//!
//! # 使用方式
//! ```rust,no_run
//! // 在内核初始化完成后调用
//! runner::executor::run_all_and_shutdown(&["/", "/musl"]);
//! ```
//!
//! 对应 Requirements 6.1-6.5（评测输出格式）和 7.1-7.6（自动测试运行器）

#![allow(dead_code)]

pub mod marker;
pub mod scanner;
pub mod executor;
pub mod shell;

// 重新导出常用接口
pub use marker::{print_group_start, print_group_end, is_valid_group, TEST_GROUPS};
pub use scanner::{TestGroup, TestResult, scan_test_scripts, is_testcode_script};
pub use executor::{run_test_group, run_all_and_shutdown};
pub use shell::{ShellParser, ShellCommand, ShellResult, find_busybox, find_sh, execute_script_auto};
