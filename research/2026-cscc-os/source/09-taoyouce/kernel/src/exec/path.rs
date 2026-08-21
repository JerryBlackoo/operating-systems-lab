use alloc::string::String;

pub(super) use super::cache::read_file_prefix_from_fs;

pub(crate) fn current_process_exec_path() -> Option<String> {
    use crate::sched::process::PROCESS_TABLE;

    let table = PROCESS_TABLE.lock();
    let table = table.as_ref()?;
    let current_pid = table.current?;
    let process = table.get(current_pid)?;
    if !process.exec_path.is_empty() {
        Some(process.exec_path.clone())
    } else if !process.name.is_empty() {
        Some(process.name.clone())
    } else {
        None
    }
}

/// Resolve an execve target path. Linux `do_execveat_common` only performs
/// normal path resolution (`do_filp_open`) on the supplied name: when the file
/// does not exist the syscall must fail with `ENOENT` so that libc's `execvp`
/// keeps walking `PATH`. The kernel must never substitute a different binary
/// that happens to share the basename.
pub(super) fn resolve_exec_path(path: &str) -> String {
    if path == "/proc/self/exe" {
        if let Some(exec_path) = current_process_exec_path() {
            if exec_path != path {
                return crate::syscall::resolve_path(&exec_path);
            }
        }
    }
    crate::syscall::resolve_path(path)
}
