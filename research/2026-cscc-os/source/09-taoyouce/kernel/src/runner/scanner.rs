//! 测试脚本扫描器
//!
//! 扫描挂载根目录中匹配 `*_testcode.sh` 模式的测试脚本文件，
//! 返回 `Vec<TestGroup>` 列表供执行引擎使用。
//!
//! # 扫描策略
//! 1. 通过 VFS 接口读取根目录（"/"）下的所有文件
//! 2. 过滤出以 `_testcode.sh` 结尾的文件
//! 3. 从文件名中提取测试组名（去掉 `_testcode.sh` 后缀）
//! 4. 按文件名排序，确保执行顺序一致
//!
//! # Requirements
//! - 7.2: 扫描挂载根目录，查找所有匹配 `*_testcode.sh` 模式的测试脚本

use alloc::string::String;
use alloc::vec::Vec;

/// 测试组信息
///
/// 每个测试组对应一个 `*_testcode.sh` 脚本文件。
#[derive(Debug, Clone)]
pub struct TestGroup {
    /// 组名（如 "basic", "busybox"）
    pub name: String,
    /// 脚本路径（如 "/basic_testcode.sh"）
    pub script_path: String,
}

/// 测试结果
#[derive(Debug, Clone)]
pub enum TestResult {
    /// 脚本执行成功（退出码 0）
    Success,
    /// 脚本执行失败（附带错误信息）
    Failed(String),
    /// 脚本执行超时
    Timeout,
    /// 脚本被跳过（文件不存在等）
    Skipped,
}

/// 测试脚本文件名后缀
const TESTCODE_SUFFIX: &str = "_testcode.sh";

/// 检查文件名是否匹配 `*_testcode.sh` 模式
///
/// # 参数
/// - `filename`: 待检查的文件名
///
/// # 返回
/// 如果文件名以 `_testcode.sh` 结尾且前缀非空，返回 `true`
///
/// # 示例
/// ```
/// assert!(is_testcode_script("basic_testcode.sh"));
/// assert!(!is_testcode_script("testcode.sh"));       // 无前缀
/// assert!(!is_testcode_script("_testcode.sh"));       // 前缀为空
/// assert!(!is_testcode_script("basic_testcode.sh.bak")); // 后缀不匹配
/// ```
pub fn is_testcode_script(filename: &str) -> bool {
    if !filename.ends_with(TESTCODE_SUFFIX) {
        return false;
    }
    // 确保前缀非空（"_testcode.sh" 本身不算）
    let prefix_len = filename.len() - TESTCODE_SUFFIX.len();
    prefix_len > 0
}

/// 从文件名中提取测试组名
///
/// 去掉 `_testcode.sh` 后缀，返回组名。
///
/// # 参数
/// - `filename`: 匹配 `*_testcode.sh` 模式的文件名
///
/// # 返回
/// 组名字符串（如 "basic"、"busybox"）
fn extract_group_name(filename: &str) -> String {
    let prefix_len = filename.len() - TESTCODE_SUFFIX.len();
    String::from(&filename[..prefix_len])
}

/// 扫描指定目录中的测试脚本
///
/// 通过 VFS 接口读取目录内容，过滤出匹配 `*_testcode.sh` 模式的文件，
/// 返回排序后的 `TestGroup` 列表。
///
/// # 参数
/// - `dir_path`: 要扫描的目录路径（如 "/"、"/musl"）
///
/// # 返回
/// - `Ok(Vec<TestGroup>)`: 找到的测试组列表（按名称排序）
/// - `Err(&'static str)`: 目录不存在或读取失败
///
/// # Requirements
/// - 7.2: 扫描挂载根目录，查找所有匹配 `*_testcode.sh` 模式的测试脚本
pub fn scan_test_scripts(dir_path: &str) -> Result<Vec<TestGroup>, &'static str> {
    use crate::fs;

    // 通过 VFS 查找目录 inode
    let dir_inode = fs::lookup(dir_path)
        .map_err(|_| "Failed to lookup scan directory")?;

    // 获取目录接口
    let dir = dir_inode.as_dir()
        .ok_or("Scan path is not a directory")?;

    // 读取目录项
    let entries = dir.read_dir()
        .map_err(|_| "Failed to read directory entries")?;

    // 过滤匹配 *_testcode.sh 的文件并构建 TestGroup
    let mut groups: Vec<TestGroup> = entries
        .iter()
        .filter(|entry| {
            entry.file_type == fs::FileType::Regular && is_testcode_script(&entry.name)
        })
        .map(|entry| {
            let name = extract_group_name(&entry.name);
            let script_path = if dir_path == "/" {
                alloc::format!("/{}", entry.name)
            } else {
                alloc::format!("{}/{}", dir_path.trim_end_matches('/'), entry.name)
            };
            TestGroup { name, script_path }
        })
        .collect();

    // 按组名排序，确保执行顺序一致
    groups.sort_by(|a, b| a.name.cmp(&b.name));

    crate::println!("[runner] Scanned {}: found {} test script(s)", dir_path, groups.len());
    for group in &groups {
        crate::println!("[runner]   - {} ({})", group.name, group.script_path);
    }

    Ok(groups)
}

/// 扫描多个目录中的测试脚本
///
/// 依次扫描给定的目录列表，合并所有找到的测试组。
/// 如果某个目录不存在或读取失败，跳过并继续。
///
/// # 参数
/// - `dirs`: 要扫描的目录路径列表
///
/// # 返回
/// 合并后的测试组列表（按名称排序）
pub fn scan_multiple_dirs(dirs: &[&str]) -> Vec<TestGroup> {
    let mut all_groups = Vec::new();

    for dir in dirs {
        match scan_test_scripts(dir) {
            Ok(groups) => all_groups.extend(groups),
            Err(e) => {
                crate::println!("[runner] Skipping {}: {}", dir, e);
            }
        }
    }

    // 去重（按脚本路径）并排序
    all_groups.sort_by(|a, b| a.name.cmp(&b.name));
    all_groups.dedup_by(|a, b| a.script_path == b.script_path);

    all_groups
}
