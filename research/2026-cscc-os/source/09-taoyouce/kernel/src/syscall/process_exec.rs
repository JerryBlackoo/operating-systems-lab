//! execve(2) syscall glue — program execution.

use alloc::string::String;
use alloc::vec::Vec;

use super::user::{read_c_string, read_path, read_user_usize};

/// Linux `MAX_ARG_STRINGS`。真实约束是 `ARG_MAX`（字节数），不是这个计数上限；
/// 原来的硬上限 256 会静默截断参数/环境更多的调用（cargo 调 rustc 常有几十个
/// 参数 + 几十个环境变量，接近甚至超过该上限）。
const MAX_ARG_STRINGS: usize = 4096;

/// 从用户态读取一个 NULL 结尾的字符串指针数组（argv / envp）。
///
/// Linux 语义（`copy_strings()` / `count()`）：**任何一个字符串复制失败都必须让
/// 整个 `execve` 失败并返回 `-EFAULT`**，绝不允许"读到一半就停下、把残缺的命令行
/// 交给新程序"。
///
/// 原实现在 `read_c_string` 出错时 `break`，于是后续参数被静默丢弃。观测后果：
/// cargo 传给 rustc 的 `--check-cfg <SPEC>` 中 `--check-cfg` 保留、紧随的 SPEC
/// 被丢掉，rustc 报 `error: Argument to option 'check-cfg' missing` → 该 crate
/// 编不出 `.rmeta` → 后续所有 crate `extern location for core does not exist`
/// → prelude 整体失效，最终表现为数百个
/// `cannot find type Result / tuple variant Some` 之类的假错误。
fn read_user_string_array(
    array_ptr: *const usize,
    what: &str,
) -> Result<Vec<String>, isize> {
    let mut out: Vec<String> = Vec::new();
    if array_ptr.is_null() || array_ptr as usize == 0 {
        return Ok(out);
    }
    for i in 0..MAX_ARG_STRINGS {
        let ptr_addr = array_ptr as usize + i * core::mem::size_of::<usize>();
        let str_ptr = match read_user_usize(ptr_addr) {
            Ok(0) => return Ok(out), // NULL 终止：正常结束
            Ok(p) => p,
            Err(_) => {
                if crate::cmdline::execdbg_enabled() {
                    crate::println!(
                        "[EXECDBG] argv-fault {} idx={} reason=read-ptr-failed -> EFAULT",
                        what, i
                    );
                }
                return Err(-14); // -EFAULT
            }
        };
        match read_c_string(str_ptr as *const u8) {
            Ok(s) => out.push(s),
            Err(e) => {
                if crate::cmdline::execdbg_enabled() {
                    crate::println!(
                        "[EXECDBG] argv-fault {} idx={} ptr={:#x} reason={} -> EFAULT",
                        what, i, str_ptr, e
                    );
                }
                return Err(-14); // -EFAULT，绝不静默截断
            }
        }
    }
    // 超过 MAX_ARG_STRINGS：Linux 返回 E2BIG。
    if crate::cmdline::execdbg_enabled() {
        crate::println!("[EXECDBG] argv-toobig {} count>{} -> E2BIG", what, MAX_ARG_STRINGS);
    }
    Err(-7) // -E2BIG
}

/// 执行程序 — 用新程序替换当前进程的地址空间。
#[allow(dead_code)]
pub(crate) fn sys_execve(path: *const u8, argv_ptr: *const usize, envp_ptr: *const usize) -> isize {
    let path_str = match read_path(path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let mut args: Vec<String> = match read_user_string_array(argv_ptr, "argv") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if args.is_empty() {
        args.push(path_str.clone());
    }

    let envp: Vec<String> = match read_user_string_array(envp_ptr, "envp") {
        Ok(v) => v,
        Err(e) => return e,
    };

    if crate::cmdline::execdbg_enabled() {
        let pid = crate::sched::process::PROCESS_TABLE
            .lock()
            .as_ref()
            .and_then(|t| t.current)
            .unwrap_or(0);
        crate::println!("[EXECDBG] enter pid={} path={} argc={}", pid, path_str, args.len());
        let rc = crate::exec::execve_from_kernel_args(path_str.clone(), args, envp);
        crate::println!("[EXECDBG] leave pid={} path={} rc={}", pid, path_str, rc);
        return rc;
    }

    crate::exec::execve_from_kernel_args(path_str, args, envp)
}

/// execve with a pre-resolved kernel path string (used by execveat).
pub(crate) fn sys_execve_with_path(
    path_str: &str, argv_ptr: *const usize, envp_ptr: *const usize,
) -> isize {
    let mut args: Vec<String> = match read_user_string_array(argv_ptr, "argv") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if args.is_empty() {
        args.push(String::from(path_str));
    }

    let envp: Vec<String> = match read_user_string_array(envp_ptr, "envp") {
        Ok(v) => v,
        Err(e) => return e,
    };

    crate::exec::execve_from_kernel_args(String::from(path_str), args, envp)
}
