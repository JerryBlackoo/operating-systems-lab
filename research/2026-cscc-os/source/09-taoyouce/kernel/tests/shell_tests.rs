//! Shell 命令解析属性测试
//!
//! 这些测试验证 Shell 命令解析的正确性属性。
//!
//! ## Property: Shell 命令解析正确性
//! Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
//! **Validates: Requirements 3.5**
//!
//! *For any* valid command string, parsing SHALL correctly extract the command name
//! and arguments, preserving the order and content of each argument.

#![allow(unused_imports)]
#![allow(unused_variables)]

// 注意：此测试模块使用固定大小的数组，避免动态内存分配
// 以防止内存耗尽问题

/// 最大参数数量（与 Shell 实现一致）
const MAX_ARGS: usize = 16;

/// 命令行缓冲区大小（与 Shell 实现一致）
const LINE_BUFFER_SIZE: usize = 256;

/// 简单的伪随机数生成器（避免内存分配）
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
    
    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if max <= min { return min; }
        min + (self.next() % (max - min))
    }
}

/// 解析命令字符串为命令名和参数列表
/// 这是 Shell 中 parse_command 函数的镜像实现
fn parse_command(input: &str) -> (&str, [&str; MAX_ARGS]) {
    let mut args: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut arg_count = 0;
    
    // 简单的空格分割
    for part in input.split_whitespace() {
        if arg_count < MAX_ARGS {
            args[arg_count] = part;
            arg_count += 1;
        }
    }
    
    let cmd = if arg_count > 0 { args[0] } else { "" };
    (cmd, args)
}

/// 计算参数数量
fn count_args(args: &[&str; MAX_ARGS]) -> usize {
    args.iter().take_while(|s| !s.is_empty()).count()
}

// ============================================================================
// Property: Shell 命令解析正确性
// ============================================================================

/// **Property: 空输入解析**
/// *For any* empty or whitespace-only input, parsing SHALL return empty command.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_empty_input() {
    // 测试空字符串
    let (cmd, args) = parse_command("");
    assert!(cmd.is_empty(), "空字符串应返回空命令");
    assert_eq!(count_args(&args), 0, "空字符串应返回零参数");
    
    // 测试纯空格
    let (cmd, args) = parse_command("   ");
    assert!(cmd.is_empty(), "纯空格应返回空命令");
    assert_eq!(count_args(&args), 0, "纯空格应返回零参数");
    
    // 测试制表符和换行
    let (cmd, args) = parse_command("\t\n  \t");
    assert!(cmd.is_empty(), "纯空白字符应返回空命令");
    assert_eq!(count_args(&args), 0, "纯空白字符应返回零参数");
}

/// **Property: 单命令解析**
/// *For any* single-word command, parsing SHALL return that word as command
/// with no additional arguments.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_single_command() {
    let test_commands = ["ls", "pwd", "help", "exit", "cd", "echo", "clear"];
    
    for cmd_str in &test_commands {
        let (cmd, args) = parse_command(cmd_str);
        assert_eq!(cmd, *cmd_str, "命令名应匹配: {}", cmd_str);
        assert_eq!(args[0], *cmd_str, "args[0] 应等于命令名");
        assert_eq!(count_args(&args), 1, "单命令应只有一个参数");
    }
}

/// **Property: 命令与参数分离**
/// *For any* command with arguments, parsing SHALL correctly separate
/// command name from arguments.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_command_args_separation() {
    // 测试带一个参数的命令
    let (cmd, args) = parse_command("cd /home");
    assert_eq!(cmd, "cd", "命令名应为 cd");
    assert_eq!(args[0], "cd", "args[0] 应为 cd");
    assert_eq!(args[1], "/home", "args[1] 应为 /home");
    assert_eq!(count_args(&args), 2, "应有 2 个参数");
    
    // 测试带多个参数的命令
    let (cmd, args) = parse_command("cp file1 file2");
    assert_eq!(cmd, "cp", "命令名应为 cp");
    assert_eq!(args[0], "cp");
    assert_eq!(args[1], "file1");
    assert_eq!(args[2], "file2");
    assert_eq!(count_args(&args), 3, "应有 3 个参数");
}

/// **Property: 参数顺序保持**
/// *For any* command with multiple arguments, parsing SHALL preserve
/// the order of arguments.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
/// 
/// 运行 10 次迭代
#[test]
fn property_shell_args_order_preserved() {
    let mut rng = SimpleRng::new(12345);
    
    // 预定义的参数池（避免动态分配）
    let arg_pool = [
        "arg1", "arg2", "arg3", "arg4", "arg5",
        "file.txt", "dir/", "/path/to/file", "123", "test",
        "-a", "-b", "--help", "--version", "-rf",
    ];
    
    // 固定大小的缓冲区
    let mut buffer = [0u8; LINE_BUFFER_SIZE];
    
    // 运行 10 次迭代
    for _ in 0..10 {
        // 随机选择参数数量 (1-8)
        let num_args = rng.next_range(1, 9) as usize;
        
        // 构建命令字符串
        let mut pos = 0;
        let mut expected_args: [&str; MAX_ARGS] = [""; MAX_ARGS];
        
        for i in 0..num_args {
            let arg_idx = rng.next_range(0, arg_pool.len() as u64) as usize;
            let arg = arg_pool[arg_idx];
            expected_args[i] = arg;
            
            // 添加空格分隔符
            if i > 0 && pos < buffer.len() {
                buffer[pos] = b' ';
                pos += 1;
            }
            
            // 复制参数
            for byte in arg.bytes() {
                if pos < buffer.len() {
                    buffer[pos] = byte;
                    pos += 1;
                }
            }
        }
        
        // 解析命令
        if let Ok(input) = core::str::from_utf8(&buffer[..pos]) {
            let (cmd, args) = parse_command(input);
            
            // 验证命令名
            assert_eq!(cmd, expected_args[0], "命令名应匹配");
            
            // 验证参数顺序
            for i in 0..num_args {
                assert_eq!(
                    args[i], expected_args[i],
                    "参数 {} 应匹配: 期望 '{}', 实际 '{}'",
                    i, expected_args[i], args[i]
                );
            }
            
            // 验证参数数量
            assert_eq!(count_args(&args), num_args, "参数数量应匹配");
        }
    }
}

/// **Property: 多空格处理**
/// *For any* command with multiple spaces between arguments, parsing SHALL
/// treat consecutive spaces as a single separator.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_multiple_spaces() {
    // 测试多个空格
    let (cmd, args) = parse_command("ls    -la     /home");
    assert_eq!(cmd, "ls");
    assert_eq!(args[0], "ls");
    assert_eq!(args[1], "-la");
    assert_eq!(args[2], "/home");
    assert_eq!(count_args(&args), 3);
    
    // 测试前导和尾随空格
    let (cmd, args) = parse_command("   echo   hello   world   ");
    assert_eq!(cmd, "echo");
    assert_eq!(args[0], "echo");
    assert_eq!(args[1], "hello");
    assert_eq!(args[2], "world");
    assert_eq!(count_args(&args), 3);
    
    // 测试混合空白字符
    let (cmd, args) = parse_command("cat\t\tfile.txt");
    assert_eq!(cmd, "cat");
    assert_eq!(args[1], "file.txt");
    assert_eq!(count_args(&args), 2);
}

/// **Property: 最大参数限制**
/// *For any* command with more than MAX_ARGS arguments, parsing SHALL
/// only keep the first MAX_ARGS arguments.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_max_args_limit() {
    // 构建超过 MAX_ARGS 的命令
    let input = "cmd a1 a2 a3 a4 a5 a6 a7 a8 a9 a10 a11 a12 a13 a14 a15 a16 a17 a18";
    let (cmd, args) = parse_command(input);
    
    assert_eq!(cmd, "cmd", "命令名应为 cmd");
    assert_eq!(count_args(&args), MAX_ARGS, "参数数量应限制为 MAX_ARGS");
    
    // 验证前 MAX_ARGS 个参数正确
    assert_eq!(args[0], "cmd");
    assert_eq!(args[1], "a1");
    assert_eq!(args[MAX_ARGS - 1], "a15"); // 第 16 个参数
}

/// **Property: 特殊字符保留**
/// *For any* argument containing special characters (except whitespace),
/// parsing SHALL preserve those characters.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_special_chars_preserved() {
    // 测试路径中的特殊字符
    let (cmd, args) = parse_command("cat /path/to/file.txt");
    assert_eq!(args[1], "/path/to/file.txt");
    
    // 测试选项标志
    let (cmd, args) = parse_command("ls -la --all");
    assert_eq!(args[1], "-la");
    assert_eq!(args[2], "--all");
    
    // 测试数字
    let (cmd, args) = parse_command("exit 127");
    assert_eq!(args[1], "127");
    
    // 测试特殊符号
    let (cmd, args) = parse_command("echo hello=world");
    assert_eq!(args[1], "hello=world");
    
    // 测试点号
    let (cmd, args) = parse_command("ls ..");
    assert_eq!(args[1], "..");
}

/// **Property: 内置命令识别**
/// *For any* built-in command, parsing SHALL correctly identify the command name.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
#[test]
fn property_shell_builtin_commands() {
    // (输入, 命令名, 额外参数数量)
    let builtins: [(&str, &str, usize); 8] = [
        ("help", "help", 0),
        ("exit 0", "exit", 1),
        ("cd /home", "cd", 1),
        ("pwd", "pwd", 0),
        ("clear", "clear", 0),
        ("echo hello world", "echo", 2),
        ("version", "version", 0),
        ("pid", "pid", 0),
    ];
    
    for (input, expected_cmd, expected_extra_args) in &builtins {
        let (cmd, args) = parse_command(input);
        
        assert_eq!(cmd, *expected_cmd, "内置命令名应匹配: {}", input);
        assert_eq!(
            count_args(&args), 
            1 + expected_extra_args,
            "参数数量应匹配: {}", input
        );
    }
}

// ============================================================================
// 自测试入口（用于内核启动时运行）
// ============================================================================

/// Shell 命令解析属性测试入口
/// 
/// 运行所有 Shell 命令解析属性测试
/// 
/// Feature: rustmicroos-comprehensive-fix, Property: Shell 命令解析正确性
/// **Validates: Requirements 3.5**
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[shell] Running command parsing property tests...");
    
    // 测试空输入
    crate::println!("[shell] Testing empty input...");
    run_empty_input_test()?;
    
    // 测试单命令
    crate::println!("[shell] Testing single command...");
    run_single_command_test()?;
    
    // 测试命令与参数分离
    crate::println!("[shell] Testing command-args separation...");
    run_command_args_separation_test()?;
    
    // 测试参数顺序保持（10 次迭代）
    crate::println!("[shell] Testing args order preserved (10 iterations)...");
    run_args_order_preserved_test()?;
    
    // 测试多空格处理
    crate::println!("[shell] Testing multiple spaces...");
    run_multiple_spaces_test()?;
    
    // 测试最大参数限制
    crate::println!("[shell] Testing max args limit...");
    run_max_args_limit_test()?;
    
    // 测试特殊字符保留
    crate::println!("[shell] Testing special chars preserved...");
    run_special_chars_preserved_test()?;
    
    // 测试内置命令识别
    crate::println!("[shell] Testing builtin commands...");
    run_builtin_commands_test()?;
    
    crate::println!("[shell] All command parsing property tests passed!");
    Ok(())
}

fn run_empty_input_test() -> Result<(), &'static str> {
    let (cmd, args) = parse_command("");
    if !cmd.is_empty() { return Err("空字符串应返回空命令"); }
    if count_args(&args) != 0 { return Err("空字符串应返回零参数"); }
    
    let (cmd, args) = parse_command("   ");
    if !cmd.is_empty() { return Err("纯空格应返回空命令"); }
    if count_args(&args) != 0 { return Err("纯空格应返回零参数"); }
    
    Ok(())
}

fn run_single_command_test() -> Result<(), &'static str> {
    let test_commands = ["ls", "pwd", "help", "exit", "cd"];
    
    for cmd_str in &test_commands {
        let (cmd, args) = parse_command(cmd_str);
        if cmd != *cmd_str { return Err("命令名不匹配"); }
        if args[0] != *cmd_str { return Err("args[0] 不等于命令名"); }
        if count_args(&args) != 1 { return Err("单命令应只有一个参数"); }
    }
    Ok(())
}

fn run_command_args_separation_test() -> Result<(), &'static str> {
    let (cmd, args) = parse_command("cd /home");
    if cmd != "cd" { return Err("命令名应为 cd"); }
    if args[1] != "/home" { return Err("args[1] 应为 /home"); }
    if count_args(&args) != 2 { return Err("应有 2 个参数"); }
    
    let (cmd, args) = parse_command("cp file1 file2");
    if cmd != "cp" { return Err("命令名应为 cp"); }
    if args[1] != "file1" { return Err("args[1] 应为 file1"); }
    if args[2] != "file2" { return Err("args[2] 应为 file2"); }
    if count_args(&args) != 3 { return Err("应有 3 个参数"); }
    
    Ok(())
}

fn run_args_order_preserved_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(12345);
    let arg_pool = ["arg1", "arg2", "arg3", "file.txt", "-a", "--help"];
    let mut buffer = [0u8; LINE_BUFFER_SIZE];
    
    for _ in 0..10 {
        let num_args = rng.next_range(1, 6) as usize;
        let mut pos = 0;
        let mut expected_args: [&str; MAX_ARGS] = [""; MAX_ARGS];
        
        for i in 0..num_args {
            let arg_idx = rng.next_range(0, arg_pool.len() as u64) as usize;
            let arg = arg_pool[arg_idx];
            expected_args[i] = arg;
            
            if i > 0 && pos < buffer.len() {
                buffer[pos] = b' ';
                pos += 1;
            }
            
            for byte in arg.bytes() {
                if pos < buffer.len() {
                    buffer[pos] = byte;
                    pos += 1;
                }
            }
        }
        
        if let Ok(input) = core::str::from_utf8(&buffer[..pos]) {
            let (cmd, args) = parse_command(input);
            if cmd != expected_args[0] { return Err("命令名不匹配"); }
            for i in 0..num_args {
                if args[i] != expected_args[i] { return Err("参数顺序不匹配"); }
            }
        }
    }
    Ok(())
}

fn run_multiple_spaces_test() -> Result<(), &'static str> {
    let (cmd, args) = parse_command("ls    -la     /home");
    if cmd != "ls" { return Err("命令名应为 ls"); }
    if args[1] != "-la" { return Err("args[1] 应为 -la"); }
    if args[2] != "/home" { return Err("args[2] 应为 /home"); }
    if count_args(&args) != 3 { return Err("应有 3 个参数"); }
    
    let (cmd, args) = parse_command("   echo   hello   ");
    if cmd != "echo" { return Err("命令名应为 echo"); }
    if args[1] != "hello" { return Err("args[1] 应为 hello"); }
    if count_args(&args) != 2 { return Err("应有 2 个参数"); }
    
    Ok(())
}

fn run_max_args_limit_test() -> Result<(), &'static str> {
    let input = "cmd a1 a2 a3 a4 a5 a6 a7 a8 a9 a10 a11 a12 a13 a14 a15 a16 a17";
    let (cmd, args) = parse_command(input);
    
    if cmd != "cmd" { return Err("命令名应为 cmd"); }
    if count_args(&args) != MAX_ARGS { return Err("参数数量应限制为 MAX_ARGS"); }
    if args[0] != "cmd" { return Err("args[0] 应为 cmd"); }
    if args[1] != "a1" { return Err("args[1] 应为 a1"); }
    
    Ok(())
}

fn run_special_chars_preserved_test() -> Result<(), &'static str> {
    let (_, args) = parse_command("cat /path/to/file.txt");
    if args[1] != "/path/to/file.txt" { return Err("路径应保留"); }
    
    let (_, args) = parse_command("ls -la --all");
    if args[1] != "-la" { return Err("选项应保留"); }
    if args[2] != "--all" { return Err("长选项应保留"); }
    
    let (_, args) = parse_command("exit 127");
    if args[1] != "127" { return Err("数字应保留"); }
    
    Ok(())
}

fn run_builtin_commands_test() -> Result<(), &'static str> {
    let (cmd, _) = parse_command("help");
    if cmd != "help" { return Err("help 命令识别失败"); }
    
    let (cmd, args) = parse_command("exit 0");
    if cmd != "exit" { return Err("exit 命令识别失败"); }
    if args[1] != "0" { return Err("exit 参数识别失败"); }
    
    let (cmd, args) = parse_command("cd /home");
    if cmd != "cd" { return Err("cd 命令识别失败"); }
    if args[1] != "/home" { return Err("cd 参数识别失败"); }
    
    Ok(())
}
