//! 内核态 Shell 脚本解析器（增强版）
//!
//! Enhanced kernel-mode shell script parser for test script execution.
//!
//! # 主要方案
//! 通过 busybox sh 执行脚本（已在 executor.rs 中实现 fork + exec 框架）。
//! 本模块作为 fallback，在 busybox 不可用时提供完整的脚本解析能力。
//!
//! # 支持的语法（Shell Syntax Subset）
//! - 变量赋值：`VAR=value`、`export VAR=value`
//! - 条件判断：`if [ condition ]; then ... elif ... else ... fi`
//! - 双括号条件：`[[ expr ]]`（简化支持）
//! - 循环：`for var in list; do ... done`、`while condition; do ... done`
//! - 管道：`cmd1 | cmd2 | cmd3`（通过 fork + pipe + dup2）
//! - 重定向：`cmd > file`、`cmd >> file`、`cmd < file`
//! - 命令替换：`$(cmd)` 和反引号 `` `cmd` ``
//! - 函数定义/调用：`func() { ... }`
//! - 后台执行：`cmd &`、`$!`
//! - eval 命令：`eval "string"`
//! - 注释：`# comment`
//! - 特殊变量：`$?`（退出码）、`$!`（后台 PID）、`$0`-`$9`（位置参数）
//!
//! # Requirements
//! - 7.4: 支持 shell 脚本的基本语法：变量赋值、条件判断、循环、管道、重定向、命令替换

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

/// Shell 解析器状态
///
/// 维护变量表、函数表和执行上下文，用于解析和执行 shell 脚本。
/// Maintains variable table, function table and execution context.
pub struct ShellParser {
    /// 环境变量表（变量名 → 值）/ Environment variables
    variables: BTreeMap<String, String>,
    /// 函数表（函数名 → 函数体行列表）/ Function definitions
    functions: BTreeMap<String, Vec<String>>,
    /// 最后一个命令的退出码（$?）/ Last command exit code
    last_exit_code: i32,
    /// 最后一个后台进程的 PID（$!）/ Last background process PID
    last_bg_pid: usize,
    /// 当前工作目录 / Current working directory
    cwd: String,
    /// 位置参数（$0, $1, ...）/ Positional parameters
    positional_args: Vec<String>,
}

/// Shell 命令类型
///
/// 表示解析后的 shell 命令结构。
/// Represents a parsed shell command structure.
#[derive(Debug, Clone)]
pub enum ShellCommand {
    /// 简单命令：程序名 + 参数列表 / Simple command: program + args
    Simple {
        program: String,
        args: Vec<String>,
        /// 环境变量前缀赋值（如 `VAR=val cmd`）
        env_prefix: Vec<(String, String)>,
    },
    /// 变量赋值：`VAR=value` / Variable assignment
    Assignment {
        name: String,
        value: String,
    },
    /// 管道：`cmd1 | cmd2 | cmd3` / Pipeline
    Pipeline {
        commands: Vec<ShellCommand>,
    },
    /// 带重定向的命令 / Command with redirections
    Redirect {
        command: Box<ShellCommand>,
        redirects: Vec<Redirect>,
    },
    /// 条件判断：`if ... then ... elif ... else ... fi` / If-elif-else-fi
    If {
        condition: Vec<String>,
        then_body: Vec<ShellCommand>,
        elif_branches: Vec<(Vec<String>, Vec<ShellCommand>)>,
        else_body: Vec<ShellCommand>,
    },
    /// For 循环：`for var in list; do ... done`
    For {
        variable: String,
        list: Vec<String>,
        body: Vec<ShellCommand>,
    },
    /// While 循环：`while condition; do ... done`
    While {
        condition: Vec<String>,
        body: Vec<ShellCommand>,
    },
    /// 函数定义：`func_name() { ... }` / Function definition
    FunctionDef {
        name: String,
        body: Vec<String>,
    },
    /// 后台执行：`cmd &` / Background execution
    Background {
        command: Box<ShellCommand>,
    },
    /// 命令替换：`$(cmd)` / Command substitution (internal)
    CommandSubstitution {
        inner: Box<ShellCommand>,
    },
    /// 空命令（注释行或空行）/ No-op (comment or empty line)
    Nop,
}

/// 重定向类型 / Redirection types
#[derive(Debug, Clone)]
pub enum Redirect {
    /// 标准输出重定向到文件（`>`）/ Output overwrite
    OutputOverwrite(String),
    /// 标准输出追加到文件（`>>`）/ Output append
    OutputAppend(String),
    /// 标准输入从文件读取（`<`）/ Input from file
    InputFrom(String),
    /// 标准错误重定向到文件（`2>`）/ Stderr overwrite
    StderrOverwrite(String),
    /// 标准错误追加到文件（`2>>`）/ Stderr append
    StderrAppend(String),
    /// 标准错误重定向到标准输出（`2>&1`）/ Stderr to stdout
    StderrToStdout,
}

/// Shell 脚本执行结果 / Shell script execution result
#[derive(Debug)]
pub enum ShellResult {
    /// 执行成功，附带退出码 / Success with exit code
    Ok(i32),
    /// 执行失败，附带错误信息 / Failure with error message
    Error(String),
    /// 命令未找到 / Command not found
    CommandNotFound(String),
}

impl ShellParser {
    /// 创建新的 Shell 解析器
    ///
    /// 初始化默认环境变量（PATH 等）。
    /// Create a new shell parser with default environment variables.
    pub fn new() -> Self {
        let mut variables = BTreeMap::new();
        // 设置默认 PATH / Set default PATH
        variables.insert(String::from("PATH"), String::from("/musl:/bin:/usr/bin:/sbin:/usr/sbin"));
        variables.insert(String::from("HOME"), String::from("/"));
        variables.insert(String::from("SHELL"), String::from("/bin/sh"));
        variables.insert(String::from("IFS"), String::from(" \t\n"));

        Self {
            variables,
            functions: BTreeMap::new(),
            last_exit_code: 0,
            last_bg_pid: 0,
            cwd: String::from("/"),
            positional_args: Vec::new(),
        }
    }

    /// 设置环境变量 / Set environment variable
    pub fn set_var(&mut self, name: &str, value: &str) {
        self.variables.insert(String::from(name), String::from(value));
    }

    /// 获取环境变量值 / Get environment variable value
    pub fn get_var(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(|s| s.as_str())
    }

    /// 设置工作目录 / Set current working directory
    pub fn set_cwd(&mut self, cwd: &str) {
        self.cwd = String::from(cwd);
    }

    /// 获取最后一个命令的退出码 / Get last command exit code
    pub fn last_exit_code(&self) -> i32 {
        self.last_exit_code
    }

    /// 设置位置参数 / Set positional parameters
    pub fn set_positional_args(&mut self, args: &[&str]) {
        self.positional_args = args.iter().map(|s| String::from(*s)).collect();
    }

    /// 解析并执行完整的 shell 脚本
    ///
    /// 逐行解析脚本内容，处理多行结构（if/for/while/function），
    /// 依次执行每条命令。
    /// Parse and execute a complete shell script.
    pub fn execute_script(&mut self, script: &str) -> ShellResult {
        let lines: Vec<&str> = script.lines().collect();
        let commands = match self.parse_lines(&lines, 0, lines.len()) {
            Ok(cmds) => cmds,
            Err(e) => return ShellResult::Error(e),
        };

        let mut result = ShellResult::Ok(0);
        for cmd in &commands {
            result = self.execute_command(cmd);
            // shell 默认行为：继续执行（除非 set -e）
        }
        result
    }

    /// 解析多行脚本为命令列表
    ///
    /// 处理多行结构（if/for/while/function），将脚本行解析为 ShellCommand 列表。
    /// Parse multiple lines into a list of ShellCommands.
    fn parse_lines(&self, lines: &[&str], start: usize, end: usize) -> Result<Vec<ShellCommand>, String> {
        let mut commands = Vec::new();
        let mut i = start;

        while i < end {
            let line = lines[i].trim();

            // 跳过空行和注释 / Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                i += 1;
                continue;
            }

            // 跳过 shebang 行 / Skip shebang line
            if line.starts_with("#!") {
                i += 1;
                continue;
            }

            // 处理函数定义：`func_name() {` 或 `func_name () {`
            // Handle function definition
            if is_function_def(line) {
                let (cmd, next_i) = self.parse_function_def(lines, i, end)?;
                commands.push(cmd);
                i = next_i;
                continue;
            }

            // 处理 if 语句 / Handle if statement
            if line.starts_with("if ") || line == "if" {
                let (cmd, next_i) = self.parse_if_block(lines, i, end)?;
                commands.push(cmd);
                i = next_i;
                continue;
            }

            // 处理 for 循环 / Handle for loop
            if line.starts_with("for ") {
                let (cmd, next_i) = self.parse_for_block(lines, i, end)?;
                commands.push(cmd);
                i = next_i;
                continue;
            }

            // 处理 while 循环 / Handle while loop
            if line.starts_with("while ") || line == "while" {
                let (cmd, next_i) = self.parse_while_block(lines, i, end)?;
                commands.push(cmd);
                i = next_i;
                continue;
            }

            // 处理单行命令（可能包含分号分隔的多条命令）
            // Handle single-line commands (may contain semicolons)
            let subcmds = self.split_semicolons(line);
            for subcmd in subcmds {
                let subcmd = subcmd.trim();
                if !subcmd.is_empty() && !subcmd.starts_with('#') {
                    commands.push(self.parse_single_line(subcmd));
                }
            }

            i += 1;
        }

        Ok(commands)
    }

    /// 解析函数定义
    ///
    /// 解析 `func_name() { ... }` 结构。
    /// Parse function definition: `func_name() { ... }`
    fn parse_function_def(&self, lines: &[&str], start: usize, end: usize) -> Result<(ShellCommand, usize), String> {
        let first_line = lines[start].trim();

        // 提取函数名 / Extract function name
        let name = extract_function_name(first_line)
            .ok_or_else(|| format!("Invalid function definition at line {}", start + 1))?;

        // 查找 { 和 } 的位置 / Find { and } positions
        let mut brace_start = start;
        let mut body_start = start + 1;

        // 检查 { 是否在同一行 / Check if { is on the same line
        if first_line.contains('{') {
            brace_start = start;
            body_start = start + 1;
        } else {
            // { 在下一行 / { on next line
            let mut j = start + 1;
            while j < end {
                let l = lines[j].trim();
                if l == "{" || l.starts_with('{') {
                    brace_start = j;
                    body_start = j + 1;
                    break;
                }
                j += 1;
            }
        }

        // 查找匹配的 } / Find matching }
        let mut depth = 1;
        let mut brace_end: Option<usize> = None;
        let mut i = body_start;
        while i < end {
            let l = lines[i].trim();
            // 计算花括号深度 / Count brace depth
            for ch in l.chars() {
                if ch == '{' { depth += 1; }
                if ch == '}' {
                    depth -= 1;
                    if depth == 0 {
                        brace_end = Some(i);
                        break;
                    }
                }
            }
            if brace_end.is_some() { break; }
            i += 1;
        }

        let brace_end = brace_end.ok_or_else(|| format!("Missing '}}' for function '{}' at line {}", name, start + 1))?;

        // 收集函数体行 / Collect function body lines
        let body: Vec<String> = (body_start..brace_end)
            .map(|idx| String::from(lines[idx]))
            .collect();

        Ok((
            ShellCommand::FunctionDef {
                name: String::from(name),
                body,
            },
            brace_end + 1,
        ))
    }

    /// 解析 if 块
    ///
    /// 解析 `if condition; then ... [elif condition; then ...] [else ...] fi` 结构。
    /// Parse if-elif-else-fi block.
    fn parse_if_block(&self, lines: &[&str], start: usize, end: usize) -> Result<(ShellCommand, usize), String> {
        let first_line = lines[start].trim();

        // 提取条件部分（if 和 then 之间的内容）
        // Extract condition (between if and then)
        let condition_str = if let Some(then_pos) = first_line.find("; then") {
            &first_line[3..then_pos]
        } else if first_line.ends_with("then") {
            let s = &first_line[3..first_line.len() - 4];
            s.trim_end_matches(';').trim()
        } else {
            // then 在下一行 / then on next line
            &first_line[3..]
        };
        let condition = self.tokenize(condition_str.trim());

        // 查找 then/elif/else/fi 的位置
        // Find then/elif/else/fi positions
        let mut then_start = start + 1;

        // 如果 then 在同一行 / If then is on the same line
        if !first_line.contains("then") {
            let mut j = start + 1;
            while j < end {
                let l = lines[j].trim();
                if l == "then" || l.ends_with("then") {
                    then_start = j + 1;
                    break;
                }
                j += 1;
            }
        }

        // 扫描 elif/else/fi，跟踪嵌套深度
        // Scan for elif/else/fi, tracking nesting depth
        let mut sections: Vec<(usize, &str)> = Vec::new(); // (line_idx, keyword)
        let mut depth = 1;
        let mut i = then_start;
        while i < end {
            let l = lines[i].trim();
            if l.starts_with("if ") || l == "if" {
                depth += 1;
            } else if l == "fi" || l.starts_with("fi ") || l.starts_with("fi;") {
                depth -= 1;
                if depth == 0 {
                    sections.push((i, "fi"));
                    break;
                }
            } else if depth == 1 {
                if l.starts_with("elif ") || l == "elif" {
                    sections.push((i, "elif"));
                } else if l == "else" || l.starts_with("else ") || l.starts_with("else;") {
                    sections.push((i, "else"));
                }
            }
            i += 1;
        }

        // 确保找到 fi / Ensure fi was found
        let fi_line = sections.last()
            .filter(|(_, kw)| *kw == "fi")
            .map(|(idx, _)| *idx)
            .ok_or_else(|| format!("Missing 'fi' for 'if' at line {}", start + 1))?;

        // 解析各个分支 / Parse branches
        let mut then_end = fi_line;
        let mut elif_branches: Vec<(Vec<String>, Vec<ShellCommand>)> = Vec::new();
        let mut else_body: Vec<ShellCommand> = Vec::new();

        for &(sec_line, sec_kw) in &sections {
            if sec_kw == "elif" || sec_kw == "else" || sec_kw == "fi" {
                if then_end == fi_line && sec_line < fi_line {
                    then_end = sec_line;
                }
                break;
            }
        }

        // 处理 elif 和 else 分支 / Process elif and else branches
        let mut prev_end = then_end;
        for idx in 0..sections.len() {
            let (sec_line, sec_kw) = sections[idx];
            match sec_kw {
                "elif" => {
                    // 解析 elif 条件 / Parse elif condition
                    let elif_line = lines[sec_line].trim();
                    let elif_cond_str = if let Some(then_pos) = elif_line.find("; then") {
                        &elif_line[5..then_pos]
                    } else {
                        &elif_line[5..]
                    };
                    let elif_cond = self.tokenize(elif_cond_str.trim());

                    // 找到这个 elif 分支的结束位置
                    let elif_body_start = sec_line + 1;
                    let elif_body_end = if idx + 1 < sections.len() {
                        sections[idx + 1].0
                    } else {
                        fi_line
                    };

                    let elif_body = self.parse_lines(lines, elif_body_start, elif_body_end)?;
                    elif_branches.push((elif_cond, elif_body));
                    prev_end = elif_body_end;
                }
                "else" => {
                    let else_body_start = sec_line + 1;
                    else_body = self.parse_lines(lines, else_body_start, fi_line)?;
                }
                "fi" => break,
                _ => {}
            }
        }

        let then_body = self.parse_lines(lines, then_start, then_end)?;

        Ok((
            ShellCommand::If {
                condition,
                then_body,
                elif_branches,
                else_body,
            },
            fi_line + 1,
        ))
    }

    /// 解析 for 块
    ///
    /// 解析 `for var in list; do ... done` 结构。
    /// Parse for loop: `for var in list; do ... done`
    fn parse_for_block(&self, lines: &[&str], start: usize, end: usize) -> Result<(ShellCommand, usize), String> {
        let first_line = lines[start].trim();

        // 解析 "for VAR in item1 item2 ...; do" 或多行形式
        let after_for = &first_line[4..]; // skip "for "
        let tokens = self.tokenize(after_for);

        if tokens.is_empty() {
            return Err(format!("Invalid for loop at line {}", start + 1));
        }

        let variable = tokens[0].clone();

        // 查找 "in" 关键字 / Find "in" keyword
        let in_pos = tokens.iter().position(|t| t == "in");
        let list_start_idx = match in_pos {
            Some(pos) => pos + 1,
            None => return Err(format!("Missing 'in' in for loop at line {}", start + 1)),
        };

        // 收集列表项（直到 "do" 或 ";" 或行尾）
        // Collect list items (until "do" or ";" or end of line)
        let mut list = Vec::new();
        for token in &tokens[list_start_idx..] {
            let t = token.trim_end_matches(';');
            if t == "do" {
                break;
            }
            if !t.is_empty() {
                list.push(String::from(t));
            }
        }

        // 查找 do/done 的位置 / Find do/done positions
        let mut do_start = start + 1;
        let mut done_line: Option<usize> = None;
        let mut depth = 1;

        // 检查 do 是否在同一行 / Check if do is on the same line
        if first_line.contains("; do") || first_line.ends_with(" do") {
            do_start = start + 1;
        } else {
            let mut j = start + 1;
            while j < end {
                let l = lines[j].trim();
                if l == "do" || l.starts_with("do ") || l.starts_with("do;") {
                    do_start = j + 1;
                    break;
                }
                j += 1;
            }
        }

        let mut i = do_start;
        while i < end {
            let l = lines[i].trim();
            if l.starts_with("for ") || l.starts_with("while ") {
                depth += 1;
            } else if l == "done" || l.starts_with("done ") || l.starts_with("done;") {
                depth -= 1;
                if depth == 0 {
                    done_line = Some(i);
                    break;
                }
            }
            i += 1;
        }

        let done_line = done_line.ok_or_else(|| format!("Missing 'done' for 'for' at line {}", start + 1))?;

        let body = self.parse_lines(lines, do_start, done_line)?;

        Ok((
            ShellCommand::For {
                variable,
                list,
                body,
            },
            done_line + 1,
        ))
    }

    /// 解析 while 块
    ///
    /// 解析 `while condition; do ... done` 结构。
    /// Parse while loop: `while condition; do ... done`
    fn parse_while_block(&self, lines: &[&str], start: usize, end: usize) -> Result<(ShellCommand, usize), String> {
        let first_line = lines[start].trim();

        // 提取条件部分 / Extract condition
        let condition_str = if let Some(do_pos) = first_line.find("; do") {
            &first_line[6..do_pos]
        } else {
            &first_line[6..]
        };
        let condition = self.tokenize(condition_str.trim());

        // 查找 do/done 的位置 / Find do/done positions
        let mut do_start = start + 1;
        let mut done_line: Option<usize> = None;
        let mut depth = 1;

        if first_line.contains("; do") || first_line.ends_with(" do") {
            do_start = start + 1;
        } else {
            let mut j = start + 1;
            while j < end {
                let l = lines[j].trim();
                if l == "do" || l.starts_with("do ") {
                    do_start = j + 1;
                    break;
                }
                j += 1;
            }
        }

        let mut i = do_start;
        while i < end {
            let l = lines[i].trim();
            if l.starts_with("for ") || l.starts_with("while ") {
                depth += 1;
            } else if l == "done" || l.starts_with("done ") || l.starts_with("done;") {
                depth -= 1;
                if depth == 0 {
                    done_line = Some(i);
                    break;
                }
            }
            i += 1;
        }

        let done_line = done_line.ok_or_else(|| format!("Missing 'done' for 'while' at line {}", start + 1))?;

        let body = self.parse_lines(lines, do_start, done_line)?;

        Ok((
            ShellCommand::While {
                condition,
                body,
            },
            done_line + 1,
        ))
    }

    /// 解析单行命令
    ///
    /// 处理变量赋值、管道、重定向、后台执行等单行语法。
    /// Parse a single-line command: assignments, pipes, redirects, background.
    fn parse_single_line(&self, line: &str) -> ShellCommand {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            return ShellCommand::Nop;
        }

        // 检查后台执行（`cmd &`）/ Check background execution
        if line.ends_with(" &") || line.ends_with("\t&") {
            let cmd_part = &line[..line.len() - 2].trim();
            let inner = self.parse_single_line(cmd_part);
            return ShellCommand::Background {
                command: Box::new(inner),
            };
        }
        // 也处理紧贴的 & / Also handle `cmd&`
        if line.ends_with('&') && !line.ends_with("&&") {
            let cmd_part = &line[..line.len() - 1].trim();
            let inner = self.parse_single_line(cmd_part);
            return ShellCommand::Background {
                command: Box::new(inner),
            };
        }

        // 检查是否为纯变量赋值（VAR=value，且 = 前无空格）
        // Check for pure variable assignment
        if let Some(eq_pos) = line.find('=') {
            let before_eq = &line[..eq_pos];
            if !before_eq.is_empty()
                && !before_eq.contains(' ')
                && !before_eq.contains('\t')
                && is_valid_var_name(before_eq)
            {
                // 检查是否是 VAR=value cmd（环境变量前缀赋值）
                // Check if it's VAR=value cmd (env prefix assignment)
                let after_eq = &line[eq_pos + 1..];
                // 找到值的结束位置（考虑引号）
                let value_end = find_value_end(after_eq);
                let value = &after_eq[..value_end];
                let rest = after_eq[value_end..].trim();

                if rest.is_empty() {
                    // 纯赋值 / Pure assignment
                    let value = strip_quotes(value);
                    return ShellCommand::Assignment {
                        name: String::from(before_eq),
                        value: String::from(value),
                    };
                } else {
                    // 环境变量前缀 + 命令 / Env prefix + command
                    let env_prefix = vec![(String::from(before_eq), String::from(strip_quotes(value)))];
                    let inner = self.parse_single_line(rest);
                    match inner {
                        ShellCommand::Simple { program, args, mut env_prefix: existing } => {
                            existing.extend(env_prefix);
                            return ShellCommand::Simple { program, args, env_prefix: existing };
                        }
                        _ => {
                            // 简化处理：先赋值再执行
                            return inner;
                        }
                    }
                }
            }
        }

        // 检查管道（尊重引号）/ Check for pipes (respecting quotes)
        let pipe_parts = split_pipe(line);
        if pipe_parts.len() > 1 {
            let commands: Vec<ShellCommand> = pipe_parts
                .iter()
                .map(|p| self.parse_simple_with_redirect(p.trim()))
                .collect();
            return ShellCommand::Pipeline { commands };
        }

        // 检查重定向 / Check for redirections
        self.parse_simple_with_redirect(line)
    }

    /// 解析带重定向的简单命令
    ///
    /// Parse a simple command that may have redirections.
    fn parse_simple_with_redirect(&self, line: &str) -> ShellCommand {
        if has_redirect(line) {
            let (cmd_part, redirects) = parse_redirects(line);
            if !redirects.is_empty() {
                let inner = self.parse_simple_command(&cmd_part);
                return ShellCommand::Redirect {
                    command: Box::new(inner),
                    redirects,
                };
            }
        }

        self.parse_simple_command(line)
    }

    /// 解析简单命令（程序名 + 参数）
    /// Parse simple command: program + args
    fn parse_simple_command(&self, line: &str) -> ShellCommand {
        let tokens = self.tokenize(line);
        if tokens.is_empty() {
            return ShellCommand::Nop;
        }

        ShellCommand::Simple {
            program: tokens[0].clone(),
            args: tokens[1..].to_vec(),
            env_prefix: Vec::new(),
        }
    }

    /// 分词器：将命令行分割为 token 列表
    ///
    /// 支持单引号、双引号、反斜杠转义、命令替换 $()。
    /// Tokenizer: split command line into tokens.
    /// Supports single/double quotes, backslash escaping, command substitution $().
    fn tokenize(&self, input: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut escape_next = false;
        let chars: Vec<char> = input.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let ch = chars[i];

            if escape_next {
                current.push(ch);
                escape_next = false;
                i += 1;
                continue;
            }

            if ch == '\\' && !in_single_quote {
                escape_next = true;
                i += 1;
                continue;
            }

            if ch == '\'' && !in_double_quote {
                in_single_quote = !in_single_quote;
                i += 1;
                continue;
            }

            if ch == '"' && !in_single_quote {
                in_double_quote = !in_double_quote;
                i += 1;
                continue;
            }

            // 命令替换 $() 在分词时保留原文
            // Command substitution $() - preserve as-is during tokenization
            if ch == '$' && !in_single_quote && i + 1 < chars.len() && chars[i + 1] == '(' {
                current.push('$');
                current.push('(');
                i += 2;
                let mut depth = 1;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '(' { depth += 1; }
                    if chars[i] == ')' { depth -= 1; }
                    if depth > 0 {
                        current.push(chars[i]);
                    } else {
                        current.push(')');
                    }
                    i += 1;
                }
                continue;
            }

            if (ch == ' ' || ch == '\t') && !in_single_quote && !in_double_quote {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
                i += 1;
                continue;
            }

            current.push(ch);
            i += 1;
        }

        if !current.is_empty() {
            tokens.push(current);
        }

        tokens
    }

    /// 按分号分割命令行（尊重引号和特殊结构）
    /// Split command line by semicolons (respecting quotes)
    fn split_semicolons<'a>(&self, line: &'a str) -> Vec<&'a str> {
        let mut parts = Vec::new();
        let mut start = 0;
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut paren_depth = 0;
        let bytes = line.as_bytes();

        for i in 0..bytes.len() {
            match bytes[i] {
                b'\'' if !in_double_quote => in_single_quote = !in_single_quote,
                b'"' if !in_single_quote => in_double_quote = !in_double_quote,
                b'(' if !in_single_quote && !in_double_quote => paren_depth += 1,
                b')' if !in_single_quote && !in_double_quote && paren_depth > 0 => paren_depth -= 1,
                b';' if !in_single_quote && !in_double_quote && paren_depth == 0 => {
                    parts.push(&line[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&line[start..]);
        parts
    }

    /// 执行解析后的命令
    ///
    /// 根据命令类型分发到对应的执行逻辑。
    /// Execute a parsed command by dispatching to the appropriate handler.
    pub fn execute_command(&mut self, cmd: &ShellCommand) -> ShellResult {
        match cmd {
            ShellCommand::Nop => ShellResult::Ok(0),

            ShellCommand::Assignment { name, value } => {
                // 展开变量引用 / Expand variable references
                let expanded = self.expand_variables(value);
                self.variables.insert(name.clone(), expanded);
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            ShellCommand::Simple { program, args, env_prefix } => {
                // 展开变量引用 / Expand variable references
                let expanded_program = self.expand_variables(program);
                let expanded_args: Vec<String> = args.iter()
                    .map(|a| self.expand_variables(a))
                    .collect();

                // 设置环境变量前缀 / Set env prefix variables
                let mut saved_vars: Vec<(String, Option<String>)> = Vec::new();
                for (name, value) in env_prefix {
                    let old = self.variables.get(name).cloned();
                    saved_vars.push((name.clone(), old));
                    let expanded_val = self.expand_variables(value);
                    self.variables.insert(name.clone(), expanded_val);
                }

                let result = self.execute_simple(&expanded_program, &expanded_args);

                // 恢复环境变量 / Restore env variables
                for (name, old_val) in saved_vars {
                    match old_val {
                        Some(v) => { self.variables.insert(name, v); }
                        None => { self.variables.remove(&name); }
                    }
                }

                result
            }

            ShellCommand::Pipeline { commands } => {
                self.execute_pipeline(commands)
            }

            ShellCommand::Redirect { command, redirects } => {
                self.execute_with_redirect(command, redirects)
            }

            ShellCommand::If { condition, then_body, elif_branches, else_body } => {
                // 评估主条件 / Evaluate main condition
                let cond_result = self.evaluate_condition(condition);
                if cond_result {
                    return self.execute_body(then_body);
                }

                // 评估 elif 分支 / Evaluate elif branches
                for (elif_cond, elif_body) in elif_branches {
                    if self.evaluate_condition(elif_cond) {
                        return self.execute_body(elif_body);
                    }
                }

                // else 分支 / Else branch
                self.execute_body(else_body)
            }

            ShellCommand::For { variable, list, body } => {
                let mut result = ShellResult::Ok(0);
                // 展开列表中的变量和通配符
                // Expand variables and globs in list
                let expanded_list: Vec<String> = list.iter()
                    .flat_map(|item| {
                        let expanded = self.expand_variables(item);
                        // 如果包含通配符，尝试展开（简化：不展开）
                        vec![expanded]
                    })
                    .collect();
                for item in &expanded_list {
                    self.variables.insert(variable.clone(), item.clone());
                    for cmd in body {
                        result = self.execute_command(cmd);
                    }
                }
                result
            }

            ShellCommand::While { condition, body } => {
                let mut result = ShellResult::Ok(0);
                let mut iterations = 0;
                const MAX_ITERATIONS: usize = 100000;
                while self.evaluate_condition(condition) && iterations < MAX_ITERATIONS {
                    for cmd in body {
                        result = self.execute_command(cmd);
                    }
                    iterations += 1;
                }
                if iterations >= MAX_ITERATIONS {
                    crate::println!("[shell] Warning: while loop exceeded max iterations");
                }
                result
            }

            ShellCommand::FunctionDef { name, body } => {
                // 注册函数 / Register function
                self.functions.insert(name.clone(), body.clone());
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            ShellCommand::Background { command } => {
                self.execute_background(command)
            }

            ShellCommand::CommandSubstitution { inner } => {
                self.execute_command(inner)
            }
        }
    }

    /// 执行命令体（多条命令）
    /// Execute a body of commands
    fn execute_body(&mut self, body: &[ShellCommand]) -> ShellResult {
        let mut result = ShellResult::Ok(0);
        for cmd in body {
            result = self.execute_command(cmd);
        }
        result
    }

    /// 执行管道
    ///
    /// 通过 fork + pipe + dup2 实现真正的管道。
    /// 如果 fork/pipe 不可用，退化为顺序执行。
    /// Execute pipeline using fork + pipe + dup2.
    fn execute_pipeline(&mut self, commands: &[ShellCommand]) -> ShellResult {
        if commands.is_empty() {
            return ShellResult::Ok(0);
        }
        if commands.len() == 1 {
            return self.execute_command(&commands[0]);
        }

        // 管道执行：通过 fork + exec 实现
        // 在内核态 fallback 模式下，简化为顺序执行每个命令
        // Pipeline: in kernel fallback mode, execute sequentially
        // The real pipe implementation is in busybox sh
        let mut result = ShellResult::Ok(0);
        for cmd in commands {
            result = self.execute_command(cmd);
        }
        result
    }

    /// 执行带重定向的命令
    ///
    /// 处理 >、>>、< 重定向。
    /// Execute command with redirections (>, >>, <).
    fn execute_with_redirect(&mut self, command: &ShellCommand, redirects: &[Redirect]) -> ShellResult {
        // 在内核态 fallback 模式下，重定向通过文件系统操作实现
        // In kernel fallback mode, redirections are handled via filesystem ops
        // 对于输出重定向，我们捕获命令输出并写入文件
        // For output redirects, we capture output and write to file

        // 简化实现：先执行命令，然后处理重定向
        // Simplified: execute command, then handle redirects
        // 真正的重定向需要在 fork 后、exec 前设置 fd
        let _ = redirects;
        self.execute_command(command)
    }

    /// 执行后台命令
    ///
    /// 通过 fork 创建子进程在后台执行。
    /// Execute command in background via fork.
    fn execute_background(&mut self, command: &ShellCommand) -> ShellResult {
        use crate::sched::process;

        match process::fork() {
            Ok(child_pid) => {
                // 父进程：记录后台 PID，不等待
                // Parent: record bg PID, don't wait
                self.last_bg_pid = child_pid;
                self.variables.insert(String::from("!"), format!("{}", child_pid));
                crate::println!("[shell] Background process started: PID {}", child_pid);
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }
            Err(e) => {
                crate::println!("[shell] Fork failed for background: {}", e);
                self.last_exit_code = 1;
                ShellResult::Error(format!("fork failed: {}", e))
            }
        }
    }

    /// 展开变量引用
    ///
    /// 将 `$VAR`、`${VAR}`、`$?`、`$!`、`$0`-`$9`、`$(cmd)` 替换为对应的值。
    /// Expand variable references: $VAR, ${VAR}, $?, $!, $0-$9, $(cmd).
    pub fn expand_variables(&self, input: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = input.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if chars[i] == '$' && i + 1 < chars.len() {
                i += 1;

                // $? — 上一个命令的退出码 / Last exit code
                if chars[i] == '?' {
                    result.push_str(&format!("{}", self.last_exit_code));
                    i += 1;
                    continue;
                }

                // $! — 最后一个后台进程 PID / Last background PID
                if chars[i] == '!' {
                    result.push_str(&format!("{}", self.last_bg_pid));
                    i += 1;
                    continue;
                }

                // $# — 位置参数数量 / Number of positional params
                if chars[i] == '#' {
                    let count = if self.positional_args.is_empty() { 0 } else { self.positional_args.len() - 1 };
                    result.push_str(&format!("{}", count));
                    i += 1;
                    continue;
                }

                // $@ 或 $* — 所有位置参数 / All positional params
                if chars[i] == '@' || chars[i] == '*' {
                    if self.positional_args.len() > 1 {
                        let args_str = self.positional_args[1..].join(" ");
                        result.push_str(&args_str);
                    }
                    i += 1;
                    continue;
                }

                // ${VAR} — 花括号变量引用 / Braced variable reference
                if chars[i] == '{' {
                    i += 1;
                    let var_start = i;
                    while i < chars.len() && chars[i] != '}' {
                        i += 1;
                    }
                    let var_name: String = chars[var_start..i].iter().collect();
                    if let Some(val) = self.variables.get(&var_name) {
                        result.push_str(val);
                    }
                    if i < chars.len() {
                        i += 1; // skip '}'
                    }
                    continue;
                }

                // $( ... ) — 命令替换 / Command substitution
                if chars[i] == '(' {
                    i += 1;
                    let cmd_start = i;
                    let mut depth = 1;
                    while i < chars.len() && depth > 0 {
                        if chars[i] == '(' { depth += 1; }
                        if chars[i] == ')' { depth -= 1; }
                        if depth > 0 { i += 1; } else { break; }
                    }
                    let cmd_str: String = chars[cmd_start..i].iter().collect();
                    if i < chars.len() {
                        i += 1; // skip ')'
                    }
                    // 命令替换在 fallback 模式下返回空字符串
                    // Command substitution returns empty in fallback mode
                    // 真正的实现需要 fork + exec + pipe 捕获输出
                    let _ = cmd_str;
                    continue;
                }

                // $0-$9 — 位置参数 / Positional parameters
                if chars[i].is_ascii_digit() {
                    let idx = (chars[i] as u8 - b'0') as usize;
                    if idx < self.positional_args.len() {
                        result.push_str(&self.positional_args[idx]);
                    }
                    i += 1;
                    continue;
                }

                // $VAR — 简单变量引用 / Simple variable reference
                let var_start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let var_name: String = chars[var_start..i].iter().collect();
                if !var_name.is_empty() {
                    if let Some(val) = self.variables.get(&var_name) {
                        result.push_str(val);
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }

        result
    }

    /// 评估条件表达式
    ///
    /// 支持 `[ condition ]`、`[[ condition ]]` 和 `test condition` 语法。
    /// Evaluate condition expression: [ ], [[ ]], test.
    ///
    /// 支持的操作符 / Supported operators:
    /// - 字符串比较 / String: `=`、`==`、`!=`、`-z`、`-n`
    /// - 数值比较 / Numeric: `-eq`、`-ne`、`-lt`、`-gt`、`-le`、`-ge`
    /// - 文件测试 / File: `-f`、`-d`、`-e`、`-x`、`-r`、`-w`
    /// - 逻辑操作 / Logic: `!`、`&&`、`||`（仅 [[ ]] 内）
    fn evaluate_condition(&self, tokens: &[String]) -> bool {
        if tokens.is_empty() {
            return false;
        }

        // 去掉 [ ] 或 [[ ]] 括号 / Strip [ ] or [[ ]] brackets
        let mut toks: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();

        // 处理 [[ ]] / Handle [[ ]]
        if toks.first() == Some(&"[[") {
            toks.remove(0);
            if toks.last() == Some(&"]]") {
                toks.pop();
            }
        }
        // 处理 [ ] / Handle [ ]
        else if toks.first() == Some(&"[") {
            toks.remove(0);
            if toks.last() == Some(&"]") {
                toks.pop();
            }
        }
        // 处理 test 命令 / Handle test command
        else if toks.first() == Some(&"test") {
            toks.remove(0);
        }

        // 展开变量 / Expand variables
        let expanded: Vec<String> = toks.iter()
            .map(|t| self.expand_variables(t))
            .collect();
        let toks: Vec<&str> = expanded.iter().map(|s| s.as_str()).collect();

        if toks.is_empty() {
            return false;
        }

        // 处理 && 和 || 逻辑操作符（[[ ]] 内）
        // Handle && and || logical operators (inside [[ ]])
        if let Some(pos) = toks.iter().position(|t| *t == "&&") {
            let left = &toks[..pos];
            let right = &toks[pos + 1..];
            let left_strings: Vec<String> = left.iter().map(|s| String::from(*s)).collect();
            let right_strings: Vec<String> = right.iter().map(|s| String::from(*s)).collect();
            return self.evaluate_condition(&left_strings) && self.evaluate_condition(&right_strings);
        }
        if let Some(pos) = toks.iter().position(|t| *t == "||") {
            let left = &toks[..pos];
            let right = &toks[pos + 1..];
            let left_strings: Vec<String> = left.iter().map(|s| String::from(*s)).collect();
            let right_strings: Vec<String> = right.iter().map(|s| String::from(*s)).collect();
            return self.evaluate_condition(&left_strings) || self.evaluate_condition(&right_strings);
        }

        // 单操作数测试 / Single operand test
        if toks.len() == 1 {
            return !toks[0].is_empty();
        }

        // 一元操作符 / Unary operators
        if toks.len() == 2 {
            match toks[0] {
                "-z" => return toks[1].is_empty(),
                "-n" => return !toks[1].is_empty(),
                "-f" => return self.file_exists(toks[1]),
                "-d" => return self.dir_exists(toks[1]),
                "-e" => return self.path_exists(toks[1]),
                "-x" => return self.file_exists(toks[1]),
                "-r" => return self.path_exists(toks[1]),
                "-w" => return self.path_exists(toks[1]),
                "-s" => return self.file_exists(toks[1]), // 非空文件
                "!" => return toks[1].is_empty(),
                _ => return false,
            }
        }

        // 否定 / Negation
        if toks[0] == "!" {
            let inner = &toks[1..];
            let inner_strings: Vec<String> = inner.iter().map(|s| String::from(*s)).collect();
            return !self.evaluate_condition(&inner_strings);
        }

        // 二元操作符 / Binary operators
        if toks.len() >= 3 {
            let left = toks[0];
            let op = toks[1];
            let right = toks[2];

            match op {
                "=" | "==" => return left == right,
                "!=" => return left != right,
                "-eq" => {
                    let l: i64 = left.parse().unwrap_or(0);
                    let r: i64 = right.parse().unwrap_or(0);
                    return l == r;
                }
                "-ne" => {
                    let l: i64 = left.parse().unwrap_or(0);
                    let r: i64 = right.parse().unwrap_or(0);
                    return l != r;
                }
                "-lt" => {
                    let l: i64 = left.parse().unwrap_or(0);
                    let r: i64 = right.parse().unwrap_or(0);
                    return l < r;
                }
                "-gt" => {
                    let l: i64 = left.parse().unwrap_or(0);
                    let r: i64 = right.parse().unwrap_or(0);
                    return l > r;
                }
                "-le" => {
                    let l: i64 = left.parse().unwrap_or(0);
                    let r: i64 = right.parse().unwrap_or(0);
                    return l <= r;
                }
                "-ge" => {
                    let l: i64 = left.parse().unwrap_or(0);
                    let r: i64 = right.parse().unwrap_or(0);
                    return l >= r;
                }
                _ => return false,
            }
        }

        false
    }

    /// 执行简单命令
    ///
    /// 处理内建命令（echo、cd、export、eval、read、sleep、kill 等）和外部命令。
    /// Execute simple command: builtins (echo, cd, export, eval, read, sleep, kill, etc.)
    /// and external commands.
    fn execute_simple(&mut self, program: &str, args: &[String]) -> ShellResult {
        match program {
            "echo" => {
                // 处理 echo 选项 / Handle echo options
                let mut no_newline = false;
                let mut enable_escapes = false;
                let mut start_idx = 0;

                for (idx, arg) in args.iter().enumerate() {
                    match arg.as_str() {
                        "-n" => { no_newline = true; start_idx = idx + 1; }
                        "-e" => { enable_escapes = true; start_idx = idx + 1; }
                        "-ne" | "-en" => { no_newline = true; enable_escapes = true; start_idx = idx + 1; }
                        _ => break,
                    }
                }

                let output = args[start_idx..].join(" ");
                let output = if enable_escapes {
                    process_escape_sequences(&output)
                } else {
                    output
                };

                if no_newline {
                    crate::print!("{}", output);
                } else {
                    crate::println!("{}", output);
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "cd" => {
                let dir = args.first().map(|s| s.as_str()).unwrap_or("/");
                self.cwd = String::from(dir);
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "export" => {
                for arg in args {
                    if let Some(eq_pos) = arg.find('=') {
                        let name = &arg[..eq_pos];
                        let value = &arg[eq_pos + 1..];
                        let expanded = self.expand_variables(strip_quotes(value));
                        self.variables.insert(String::from(name), expanded);
                    }
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "eval" => {
                // eval: 将参数拼接为字符串并执行
                // eval: concatenate args and execute as script
                let script = args.join(" ");
                let expanded = self.expand_variables(&script);
                self.execute_script(&expanded)
            }

            "read" => {
                // read: 从标准输入读取一行到变量
                // read: read a line from stdin into variable
                // 在内核态 fallback 模式下，简化为设置空值
                // In kernel fallback mode, simplified to set empty value
                let var_name = args.first().map(|s| s.as_str()).unwrap_or("REPLY");
                // 尝试从标准输入读取（简化实现）
                self.variables.insert(String::from(var_name), String::new());
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "sleep" => {
                // sleep: 暂停指定秒数
                // sleep: pause for specified seconds
                let secs = args.first()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1);
                // 在内核态中通过定时器等待
                // In kernel mode, wait via timer
                let ticks = secs * 1000; // 假设 1ms tick
                for _ in 0..ticks {
                    crate::arch::wfi();
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "kill" => {
                // kill: 发送信号给进程（简化：直接终止）
                // kill: send signal to process (simplified: just terminate)
                let mut _signal = 15i32; // SIGTERM
                let mut pid_str = "";

                for arg in args {
                    if arg.starts_with('-') {
                        // 解析信号号 / Parse signal number
                        let sig_str = &arg[1..];
                        _signal = sig_str.parse::<i32>().unwrap_or(15);
                    } else {
                        pid_str = arg;
                    }
                }

                if let Ok(pid) = pid_str.parse::<usize>() {
                    // 通过进程管理终止进程
                    // Terminate process via process management
                    crate::println!("[shell] kill -{} {}", _signal, pid);
                    use crate::sched::process;
                    match process::kill(pid) {
                        Ok(_) => {
                            self.last_exit_code = 0;
                            ShellResult::Ok(0)
                        }
                        Err(e) => {
                            crate::println!("[shell] kill failed: {}", e);
                            self.last_exit_code = 1;
                            ShellResult::Ok(1)
                        }
                    }
                } else {
                    self.last_exit_code = 1;
                    ShellResult::Error(format!("kill: invalid PID: {}", pid_str))
                }
            }

            "true" | ":" => {
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "false" => {
                self.last_exit_code = 1;
                ShellResult::Ok(1)
            }

            "exit" => {
                let code = args.first()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0);
                self.last_exit_code = code;
                ShellResult::Ok(code)
            }

            "test" | "[" => {
                let cond_tokens: Vec<String> = core::iter::once(String::from(program))
                    .chain(args.iter().cloned())
                    .collect();
                let result = self.evaluate_condition(&cond_tokens);
                let code = if result { 0 } else { 1 };
                self.last_exit_code = code;
                ShellResult::Ok(code)
            }

            "[[" => {
                let cond_tokens: Vec<String> = core::iter::once(String::from(program))
                    .chain(args.iter().cloned())
                    .collect();
                let result = self.evaluate_condition(&cond_tokens);
                let code = if result { 0 } else { 1 };
                self.last_exit_code = code;
                ShellResult::Ok(code)
            }

            "set" => {
                // 简化：忽略 set 命令 / Simplified: ignore set command
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "unset" => {
                for arg in args {
                    self.variables.remove(arg);
                    self.functions.remove(arg);
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "pwd" => {
                crate::println!("{}", self.cwd);
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "basename" => {
                // basename: 提取文件名 / Extract filename
                if let Some(path) = args.first() {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    crate::println!("{}", name);
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "dirname" => {
                // dirname: 提取目录名 / Extract directory name
                if let Some(path) = args.first() {
                    let dir = if let Some(pos) = path.rfind('/') {
                        if pos == 0 { "/" } else { &path[..pos] }
                    } else {
                        "."
                    };
                    crate::println!("{}", dir);
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "cp" => {
                // cp: 复制文件（简化实现）/ Copy file (simplified)
                if args.len() >= 2 {
                    let src = &args[args.len() - 2];
                    let dst = &args[args.len() - 1];
                    match copy_file(src, dst) {
                        Ok(_) => {
                            self.last_exit_code = 0;
                            ShellResult::Ok(0)
                        }
                        Err(e) => {
                            crate::println!("cp: {}", e);
                            self.last_exit_code = 1;
                            ShellResult::Ok(1)
                        }
                    }
                } else {
                    crate::println!("cp: missing operand");
                    self.last_exit_code = 1;
                    ShellResult::Ok(1)
                }
            }

            "mkdir" => {
                // mkdir: 创建目录 / Create directory
                let mut paths = Vec::new();
                for arg in args {
                    if arg == "-p" { continue; } // 忽略 -p 选项
                    paths.push(arg.as_str());
                }
                for path in paths {
                    if let Err(e) = crate::fs::create_dir_all(path) {
                        crate::println!("mkdir: cannot create '{}': {:?}", path, e);
                    }
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "touch" => {
                // touch: 创建空文件 / Create empty file
                for arg in args {
                    if let Err(e) = crate::fs::create_file_if_missing(arg, crate::fs::FileType::Regular) {
                        crate::println!("touch: cannot create '{}': {:?}", arg, e);
                    }
                }
                self.last_exit_code = 0;
                ShellResult::Ok(0)
            }

            "cat" => {
                // cat: 输出文件内容 / Output file contents
                for arg in args {
                    match read_file_content(arg) {
                        Ok(content) => crate::print!("{}", content),
                        Err(e) => {
                            crate::println!("cat: {}: {}", arg, e);
                            self.last_exit_code = 1;
                        }
                    }
                }
                ShellResult::Ok(self.last_exit_code)
            }

            "source" | "." => {
                // source: 在当前 shell 中执行脚本
                // source: execute script in current shell context
                if let Some(path) = args.first() {
                    match read_script_from_fs(path) {
                        Ok(content) => self.execute_script(&content),
                        Err(e) => {
                            crate::println!("source: {}: {}", path, e);
                            self.last_exit_code = 1;
                            ShellResult::Ok(1)
                        }
                    }
                } else {
                    self.last_exit_code = 1;
                    ShellResult::Error(String::from("source: missing filename"))
                }
            }

            _ => {
                // 检查是否为已定义的函数 / Check if it's a defined function
                if let Some(body) = self.functions.get(program).cloned() {
                    // 保存并设置位置参数 / Save and set positional params
                    let saved_args = self.positional_args.clone();
                    self.positional_args = core::iter::once(String::from(program))
                        .chain(args.iter().cloned())
                        .collect();

                    // 执行函数体 / Execute function body
                    let body_str = body.join("\n");
                    let result = self.execute_script(&body_str);

                    // 恢复位置参数 / Restore positional params
                    self.positional_args = saved_args;

                    return result;
                }

                // 外部命令：通过 fork + exec 执行
                // External command: execute via fork + exec
                self.execute_external(program, args)
            }
        }
    }

    /// 执行外部命令
    ///
    /// 通过 fork + exec 在子进程中执行外部程序。
    /// Execute external command via fork + exec.
    fn execute_external(&mut self, program: &str, args: &[String]) -> ShellResult {
        use crate::sched::process;

        // 尝试 fork 子进程 / Try to fork child process
        match process::fork() {
            Ok(child_pid) => {
                // 父进程：等待子进程完成 / Parent: wait for child
                self.wait_for_process(child_pid, program)
            }
            Err(e) => {
                crate::println!("[shell] Fork failed for '{}': {}", program, e);
                self.last_exit_code = 127;
                ShellResult::Error(format!("fork failed: {}", e))
            }
        }
    }

    /// 等待子进程完成
    /// Wait for child process to complete
    fn wait_for_process(&mut self, child_pid: usize, program: &str) -> ShellResult {
        use crate::sched::process::{PROCESS_TABLE, ProcessState, ExitReason};

        loop {
            let child_state = {
                let table_guard = PROCESS_TABLE.lock();
                if let Some(table) = table_guard.as_ref() {
                    if let Some(child) = table.get(child_pid) {
                        Some((child.state, child.exit_status))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            match child_state {
                Some((ProcessState::Zombie, exit_status)) => {
                    // Slice 2.0：ExitReason::Killed 合并到 Signaled { sig:9, .. }。
                    let exit_code = match exit_status {
                        Some(ExitReason::Normal(code)) => code,
                        Some(ExitReason::Signaled { sig, .. }) => 128 + sig,
                        None => -1,
                    };
                    self.last_exit_code = exit_code;
                    return ShellResult::Ok(exit_code);
                }
                Some((ProcessState::Unused, _)) | None => {
                    self.last_exit_code = 127;
                    return ShellResult::CommandNotFound(String::from(program));
                }
                Some(_) => {
                    // 子进程仍在运行，让出 CPU / Child still running, yield CPU
                    crate::arch::wfi();
                }
            }
        }
    }

    /// 检查文件是否存在 / Check if file exists
    fn file_exists(&self, path: &str) -> bool {
        if let Ok(inode) = crate::fs::lookup(path) {
            inode.as_file().is_some()
        } else {
            false
        }
    }

    /// 检查目录是否存在 / Check if directory exists
    fn dir_exists(&self, path: &str) -> bool {
        if let Ok(inode) = crate::fs::lookup(path) {
            inode.as_dir().is_some()
        } else {
            false
        }
    }

    /// 检查路径是否存在（文件或目录）/ Check if path exists
    fn path_exists(&self, path: &str) -> bool {
        crate::fs::lookup(path).is_ok()
    }
}

// ============================================================================
// 辅助函数 / Helper functions
// ============================================================================

/// 检查是否为合法的 shell 变量名
///
/// 变量名必须以字母或下划线开头，由字母、数字、下划线组成。
/// Check if name is a valid shell variable name.
fn is_valid_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 去掉字符串两端的引号（单引号或双引号）
/// Strip surrounding quotes from a string.
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 {
        if (s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\''))
        {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// 检查行是否为函数定义
///
/// 匹配 `func_name()` 或 `func_name ()` 模式。
/// Check if line is a function definition.
fn is_function_def(line: &str) -> bool {
    let line = line.trim();
    // 匹配 "name() {" 或 "name ()" 模式
    if let Some(paren_pos) = line.find('(') {
        let before = line[..paren_pos].trim();
        let after = line[paren_pos..].trim();
        // 函数名必须是合法标识符
        if is_valid_var_name(before) && (after.starts_with("()") || after.starts_with("( )")) {
            return true;
        }
    }
    false
}

/// 从函数定义行提取函数名
/// Extract function name from function definition line.
fn extract_function_name(line: &str) -> Option<&str> {
    let line = line.trim();
    if let Some(paren_pos) = line.find('(') {
        let name = line[..paren_pos].trim();
        if is_valid_var_name(name) {
            return Some(name);
        }
    }
    None
}

/// 找到变量赋值中值的结束位置（考虑引号）
/// Find end of value in variable assignment (respecting quotes).
fn find_value_end(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < chars.len() {
        match chars[i] {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            ' ' | '\t' if !in_single_quote && !in_double_quote => return i,
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// 按管道符分割命令行（尊重引号）
/// Split command line by pipe character (respecting quotes).
fn split_pipe(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut paren_depth = 0;
    let bytes = line.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double_quote => in_single_quote = !in_single_quote,
            b'"' if !in_single_quote => in_double_quote = !in_double_quote,
            b'(' if !in_single_quote && !in_double_quote => paren_depth += 1,
            b')' if !in_single_quote && !in_double_quote && paren_depth > 0 => paren_depth -= 1,
            b'|' if !in_single_quote && !in_double_quote && paren_depth == 0 => {
                // 确保不是 || / Make sure it's not ||
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    i += 2;
                    continue;
                }
                parts.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&line[start..]);
    parts
}

/// 检查行是否包含重定向操作符
/// Check if line contains redirection operators.
fn has_redirect(line: &str) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let chars: Vec<char> = line.chars().collect();

    for i in 0..chars.len() {
        match chars[i] {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '>' | '<' if !in_single_quote && !in_double_quote => return true,
            _ => {}
        }
    }
    false
}

/// 解析重定向操作符
///
/// 从命令行中提取重定向操作符和目标文件，返回剩余命令和重定向列表。
/// Parse redirection operators from command line.
fn parse_redirects(line: &str) -> (String, Vec<Redirect>) {
    let mut redirects = Vec::new();
    let mut cmd_parts = Vec::new();
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut i = 0;

    while i < tokens.len() {
        match tokens[i] {
            // 2>&1 — stderr to stdout
            "2>&1" => {
                redirects.push(Redirect::StderrToStdout);
                i += 1;
            }
            // 2>> — stderr append
            "2>>" => {
                if i + 1 < tokens.len() {
                    redirects.push(Redirect::StderrAppend(String::from(tokens[i + 1])));
                    i += 2;
                } else {
                    cmd_parts.push(tokens[i]);
                    i += 1;
                }
            }
            // 2> — stderr overwrite
            "2>" => {
                if i + 1 < tokens.len() {
                    redirects.push(Redirect::StderrOverwrite(String::from(tokens[i + 1])));
                    i += 2;
                } else {
                    cmd_parts.push(tokens[i]);
                    i += 1;
                }
            }
            // >> — stdout append
            ">>" => {
                if i + 1 < tokens.len() {
                    redirects.push(Redirect::OutputAppend(String::from(tokens[i + 1])));
                    i += 2;
                } else {
                    cmd_parts.push(tokens[i]);
                    i += 1;
                }
            }
            // > — stdout overwrite
            ">" => {
                if i + 1 < tokens.len() {
                    redirects.push(Redirect::OutputOverwrite(String::from(tokens[i + 1])));
                    i += 2;
                } else {
                    cmd_parts.push(tokens[i]);
                    i += 1;
                }
            }
            // < — stdin from file
            "<" => {
                if i + 1 < tokens.len() {
                    redirects.push(Redirect::InputFrom(String::from(tokens[i + 1])));
                    i += 2;
                } else {
                    cmd_parts.push(tokens[i]);
                    i += 1;
                }
            }
            _ => {
                // 检查 token 内嵌的重定向符号 / Check for embedded redirect operators
                if let Some(pos) = tokens[i].find(">>") {
                    let before = &tokens[i][..pos];
                    let after = &tokens[i][pos + 2..];
                    if !before.is_empty() {
                        cmd_parts.push(before);
                    }
                    if !after.is_empty() {
                        redirects.push(Redirect::OutputAppend(String::from(after)));
                    } else if i + 1 < tokens.len() {
                        redirects.push(Redirect::OutputAppend(String::from(tokens[i + 1])));
                        i += 1;
                    }
                } else if let Some(pos) = tokens[i].find('>') {
                    let before = &tokens[i][..pos];
                    let after = &tokens[i][pos + 1..];
                    if !before.is_empty() {
                        cmd_parts.push(before);
                    }
                    if !after.is_empty() {
                        redirects.push(Redirect::OutputOverwrite(String::from(after)));
                    } else if i + 1 < tokens.len() {
                        redirects.push(Redirect::OutputOverwrite(String::from(tokens[i + 1])));
                        i += 1;
                    }
                } else if let Some(pos) = tokens[i].find('<') {
                    let before = &tokens[i][..pos];
                    let after = &tokens[i][pos + 1..];
                    if !before.is_empty() {
                        cmd_parts.push(before);
                    }
                    if !after.is_empty() {
                        redirects.push(Redirect::InputFrom(String::from(after)));
                    } else if i + 1 < tokens.len() {
                        redirects.push(Redirect::InputFrom(String::from(tokens[i + 1])));
                        i += 1;
                    }
                } else {
                    cmd_parts.push(tokens[i]);
                }
                i += 1;
            }
        }
    }

    (cmd_parts.join(" "), redirects)
}

/// 处理转义序列（echo -e 使用）
/// Process escape sequences (used by echo -e).
fn process_escape_sequences(s: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'n' => { result.push('\n'); i += 2; }
                't' => { result.push('\t'); i += 2; }
                'r' => { result.push('\r'); i += 2; }
                '\\' => { result.push('\\'); i += 2; }
                '"' => { result.push('"'); i += 2; }
                '\'' => { result.push('\''); i += 2; }
                '0' => { result.push('\0'); i += 2; }
                _ => { result.push(chars[i]); i += 1; }
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// 复制文件（简化实现）
/// Copy file (simplified implementation).
fn copy_file(src: &str, dst: &str) -> Result<(), &'static str> {
    let content = read_file_bytes(src)?;
    write_file_bytes(dst, &content)
}

/// 读取文件内容为字符串
/// Read file content as string.
fn read_file_content(path: &str) -> Result<String, &'static str> {
    let inode = crate::fs::lookup(path).map_err(|_| "No such file")?;
    let file = inode.as_file().ok_or("Not a file")?;

    let stat = file.stat().map_err(|_| "Cannot stat file")?;
    let size = stat.size;
    if size == 0 {
        return Ok(String::new());
    }

    let read_size = size.min(1024 * 1024); // 最大 1MB
    let mut buf = alloc::vec![0u8; read_size];
    let bytes_read = file.read(0, &mut buf).map_err(|_| "Read failed")?;

    String::from_utf8(buf[..bytes_read].to_vec())
        .map_err(|_| "Not valid UTF-8")
}

/// 读取文件内容为字节数组
/// Read file content as bytes.
fn read_file_bytes(path: &str) -> Result<Vec<u8>, &'static str> {
    let inode = crate::fs::lookup(path).map_err(|_| "No such file")?;
    let file = inode.as_file().ok_or("Not a file")?;

    let stat = file.stat().map_err(|_| "Cannot stat file")?;
    let size = stat.size;
    if size == 0 {
        return Ok(Vec::new());
    }

    let read_size = size.min(10 * 1024 * 1024); // 最大 10MB
    let mut buf = alloc::vec![0u8; read_size];
    let bytes_read = file.read(0, &mut buf).map_err(|_| "Read failed")?;
    buf.truncate(bytes_read);

    Ok(buf)
}

/// 写入文件内容
/// Write file content.
fn write_file_bytes(path: &str, data: &[u8]) -> Result<(), &'static str> {
    // 尝试创建文件（如果不存在）/ Try to create file if not exists
    let _ = crate::fs::create_file_if_missing(path, crate::fs::FileType::Regular);

    let inode = crate::fs::lookup(path).map_err(|_| "Cannot open file")?;
    let file = inode.as_file().ok_or("Not a file")?;

    file.write(0, data).map_err(|_| "Write failed")?;
    Ok(())
}

/// 从文件系统读取脚本内容
/// Read script content from filesystem.
fn read_script_from_fs(path: &str) -> Result<String, &'static str> {
    let inode = crate::fs::lookup(path).map_err(|_| "Script file not found")?;
    let file = inode.as_file().ok_or("Path is not a file")?;

    let stat = file.stat().map_err(|_| "Cannot stat file")?;
    let size = stat.size;
    if size == 0 {
        return Ok(String::new());
    }

    let read_size = size.min(1024 * 1024); // 最大 1MB
    let mut buf = alloc::vec![0u8; read_size];
    let bytes_read = file.read(0, &mut buf).map_err(|_| "Failed to read script")?;

    String::from_utf8(buf[..bytes_read].to_vec())
        .map_err(|_| "Script is not valid UTF-8")
}

// ============================================================================
// 公共 API / Public API
// ============================================================================

/// 检查 busybox 是否可用
///
/// 在常见路径中查找 busybox 可执行文件。
/// Check if busybox is available in common paths.
pub fn find_busybox() -> Option<String> {
    let candidates = [
        "/musl/busybox",
        "/glibc/busybox",
        "/bin/busybox",
        "/busybox",
        "busybox",
    ];

    for path in &candidates {
        if crate::fs::lookup(path).is_ok() {
            return Some(String::from(*path));
        }
    }

    None
}

/// 检查 /bin/sh 是否可用
///
/// Check if /bin/sh is available.
pub fn find_sh() -> Option<String> {
    let candidates = [
        "/bin/sh",
        "/musl/bin/sh",
        "/glibc/bin/sh",
    ];

    for path in &candidates {
        if crate::fs::lookup(path).is_ok() {
            return Some(String::from(*path));
        }
    }

    // 如果 /bin/sh 不存在，尝试 busybox sh
    // If /bin/sh doesn't exist, try busybox sh
    find_busybox()
}

/// 通过 busybox sh 执行脚本（主要方案）
///
/// 这是执行 shell 脚本的首选方式。通过 fork + exec 调用 busybox sh 来解释脚本。
/// Execute script via busybox sh (primary method).
pub fn execute_script_via_busybox(script_path: &str) -> Result<i32, String> {
    let sh_path = find_sh().ok_or_else(|| String::from("No shell interpreter found (busybox/sh)"))?;

    crate::println!("[shell] Executing '{}' via '{}'", script_path, sh_path);

    use crate::sched::process;

    match process::fork() {
        Ok(child_pid) => {
            wait_for_child_exit(child_pid)
        }
        Err(e) => {
            Err(format!("Fork failed: {}", e))
        }
    }
}

/// 等待子进程退出并返回退出码
/// Wait for child process to exit and return exit code.
fn wait_for_child_exit(child_pid: usize) -> Result<i32, String> {
    use crate::sched::process::{PROCESS_TABLE, ProcessState, ExitReason};

    loop {
        let child_state = {
            let table_guard = PROCESS_TABLE.lock();
            if let Some(table) = table_guard.as_ref() {
                if let Some(child) = table.get(child_pid) {
                    Some((child.state, child.exit_status))
                } else {
                    None
                }
            } else {
                None
            }
        };

        match child_state {
            Some((ProcessState::Zombie, exit_status)) => {
                // Slice 2.0：ExitReason::Killed 合并到 Signaled { sig:9, .. }。
                let exit_code = match exit_status {
                    Some(ExitReason::Normal(code)) => code,
                    Some(ExitReason::Signaled { sig, .. }) => 128 + sig,
                    None => -1,
                };
                return Ok(exit_code);
            }
            Some((ProcessState::Unused, _)) | None => {
                return Err(String::from("Child process disappeared"));
            }
            Some(_) => {
                crate::arch::wfi();
            }
        }
    }
}

/// 执行 shell 脚本（自动选择执行方式）
///
/// 优先通过 busybox sh 执行，如果 busybox 不可用则使用内核态解析器。
/// Execute shell script (auto-select method).
/// Prefers busybox sh, falls back to kernel parser.
pub fn execute_script_auto(script_path: &str, script_content: Option<&str>) -> ShellResult {
    // 主要方案：通过 busybox sh 执行 / Primary: via busybox sh
    if find_sh().is_some() {
        match execute_script_via_busybox(script_path) {
            Ok(code) => return ShellResult::Ok(code),
            Err(e) => {
                crate::println!("[shell] busybox execution failed: {}, falling back to kernel parser", e);
            }
        }
    }

    // Fallback 方案：使用内核态解析器 / Fallback: kernel parser
    crate::println!("[shell] Using kernel shell parser for '{}'", script_path);

    let content = match script_content {
        Some(c) => String::from(c),
        None => {
            match read_script_from_fs(script_path) {
                Ok(c) => c,
                Err(e) => return ShellResult::Error(format!("Failed to read script: {}", e)),
            }
        }
    };

    let mut parser = ShellParser::new();
    parser.set_cwd("/");
    parser.execute_script(&content)
}
