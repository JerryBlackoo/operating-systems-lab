use alloc::string::String;
use alloc::vec::Vec;

use crate::syscall::SyscallError;

use super::path::{read_file_prefix_from_fs, resolve_exec_path};

pub(super) struct ExecRequest {
    pub(super) exec_target: String,
    pub(super) exec_name: String,
    pub(super) exec_args: Vec<String>,
}

pub(super) fn resolve_exec_request(path_str: &str, args: Vec<String>) -> Result<ExecRequest, SyscallError> {
    let resolved = resolve_exec_path(path_str);
    let file_prefix_result = read_file_prefix_from_fs(&resolved, 256);
    let (exec_target, exec_name, exec_args) = match file_prefix_result {
        Ok(file_prefix) => {
            let is_shebang = file_prefix.len() >= 2
                && file_prefix[0] == b'#'
                && file_prefix[1] == b'!';
            let is_elf = file_prefix.len() >= 4
                && file_prefix[0] == 0x7f
                && file_prefix[1] == b'E'
                && file_prefix[2] == b'L'
                && file_prefix[3] == b'F';

            if is_shebang {
                let first_line_end = file_prefix[2..].iter().position(|&b| b == b'\n').unwrap_or(file_prefix.len().saturating_sub(2));
                let shebang_line = core::str::from_utf8(&file_prefix[2..2 + first_line_end]).unwrap_or("");
                let shebang_line = shebang_line.trim().trim_end_matches('\r');

                let (interp_path, interp_arg) = match shebang_line.find(|c: char| c == ' ' || c == '\t') {
                    Some(pos) => (&shebang_line[..pos], Some(shebang_line[pos..].trim())),
                    None => (shebang_line, None),
                };

                if interp_path.is_empty() {
                    return Err(SyscallError::InvalidArgument);
                }

                // Linux `binfmt_script.c` resolves the interpreter with a plain
                // `open_exec`: a missing interpreter fails the whole execve with
                // ENOENT. Substituting a different shell here would silently run
                // the script under an interpreter it was not written for.
                let interp_resolved = resolve_exec_path(interp_path);
                if read_file_prefix_from_fs(&interp_resolved, 4).is_err() {
                    return Err(SyscallError::NotFound);
                }

                let mut new_args: Vec<String> = Vec::new();
                new_args.push(String::from(interp_path));
                if let Some(arg) = interp_arg {
                    if !arg.is_empty() {
                        new_args.push(String::from(arg));
                    }
                }
                new_args.push(resolved.clone());
                if args.len() > 1 {
                    for a in &args[1..] {
                        new_args.push(a.clone());
                    }
                }

                (interp_resolved, String::from(interp_path), new_args)
            } else if !is_elf {
                // 文件可读但既非 ELF 又无 shebang：Linux execve 直接返回
                // ENOEXEC（kernel 不替用户态 fallback 到 shell）。真正要把脚本
                // 跑起来的场景（libc execvp 对 ENOEXEC 做 `/bin/sh` 重试）都在
                // 用户态处理，语义不受影响。
                return Err(SyscallError::ExecFormat);
            } else {
                (resolved.clone(), String::from(path_str), args)
            }
        }
        Err(_) => {
            // Linux: a nonexistent/unreadable execve target is ENOENT. glibc's
            // `execvp` relies on this to continue walking `PATH`; picking a
            // same-basename binary from another directory would hijack the
            // search (observed: `/usr/bin/env bash` running BusyBox ash because
            // the missing `/usr/local/bin/bash` was substituted).
            return Err(SyscallError::NotFound);
        }
    };

    Ok(ExecRequest { exec_target, exec_name, exec_args })
}
