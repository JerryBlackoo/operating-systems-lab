//! 评测标记输出模块
//!
//! 输出 `#### OS COMP TEST GROUP START/END ####` 格式的标记，
//! 供官方评分脚本（os-comp-judge）解析和评分。
//!
//! 标记格式必须与官方评分脚本完全匹配，START/END 成对出现，
//! `<group_name>` 完全一致。

#![allow(dead_code)]

/// 支持的 12 组测试名称
///
/// 对应官方评测系统的 12 个测试组，涵盖基础功能、性能基准、网络和实时性测试。
pub const TEST_GROUPS: &[&str] = &[
    "basic",       // 基础系统调用测试
    "busybox",     // BusyBox 工具集测试
    "lua",         // Lua 脚本引擎测试
    "libctest",    // C 标准库兼容性测试
    "libcbench",   // C 标准库性能基准
    "iozone",      // 文件系统 I/O 性能测试
    "unixbench",   // Unix 综合性能基准
    "lmbench",     // 系统调用延迟基准
    "iperf",       // 网络吞吐量测试
    "netperf",     // 网络性能测试
    "cyclictest",  // 实时性延迟测试
    "ltp",         // Linux Test Project 兼容性测试
];

/// 输出测试组开始标记
///
/// 格式: `#### OS COMP TEST GROUP START <name> ####`
///
/// # 参数
/// - `name`: 测试组名称，应为 `TEST_GROUPS` 中的某个值
///
/// # 示例
/// ```
/// print_group_start("basic");
/// // 输出: #### OS COMP TEST GROUP START basic ####
/// ```
pub fn print_group_start(name: &str) {
    crate::println!("#### OS COMP TEST GROUP START {} ####", name);
}

/// 输出测试组结束标记
///
/// 格式: `#### OS COMP TEST GROUP END <name> ####`
///
/// # 参数
/// - `name`: 测试组名称，必须与对应的 `print_group_start` 调用中的名称完全一致
///
/// # 示例
/// ```
/// print_group_end("basic");
/// // 输出: #### OS COMP TEST GROUP END basic ####
/// ```
pub fn print_group_end(name: &str) {
    crate::println!("#### OS COMP TEST GROUP END {} ####", name);
}

/// 检查给定名称是否为有效的测试组名称
///
/// # 参数
/// - `name`: 待检查的测试组名称
///
/// # 返回
/// 如果名称在 `TEST_GROUPS` 中则返回 `true`，否则返回 `false`
pub fn is_valid_group(name: &str) -> bool {
    TEST_GROUPS.contains(&name)
}

/// 输出所有测试组的开始和结束标记（用于调试/验证）
///
/// 依次为每个测试组输出 START 和 END 标记，中间无内容。
/// 主要用于验证标记格式是否正确。
pub fn print_all_markers() {
    for group in TEST_GROUPS {
        print_group_start(group);
        print_group_end(group);
    }
}
