use alloc::string::String;
use alloc::vec::Vec;

use crate::loader::{ElfInfo, ElfLoader};
use crate::mm::AddressSpace;
use crate::syscall::SyscallError;

use super::error::ExecError;
use super::process::replace_current_process_address_space;
use super::stack::{allocate_user_stack, push_args_to_stack};

fn load_elf_from_fs(path: &str) -> Result<Vec<u8>, SyscallError> {
    use crate::fs;

    let inode = fs::lookup(path).map_err(|_| SyscallError::NotFound)?;

    let stat = inode.stat().map_err(|_| SyscallError::InvalidArgument)?;
    let size = stat.size;

    if size == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let file = inode.as_file().ok_or(SyscallError::InvalidArgument)?;
    let mut buf = alloc::vec![0u8; size];
    let read_len = file.read(0, &mut buf).map_err(|_| SyscallError::InvalidArgument)?;

    if read_len != size {
        return Err(SyscallError::InvalidArgument);
    }

    Ok(buf)
}

/// Build interpreter path candidates for a PT_INTERP string.
///
/// The competition sdcard image puts dynamic loaders under split sub-trees
/// (`/musl/lib/...`, `/glibc/lib/...`, `/glibc/lib64/...`), but produced binaries
/// embed absolute paths like `/lib/ld-musl-riscv64-sf.so.1` or
/// `/lib/ld-linux-riscv64-lp64d.so.1` in their PT_INTERP segment. Since the VFS
/// `lookup` resolves absolute paths against the real root and ignores process
/// CWD, we must rewrite the requested interp path into the actual on-disk
/// locations.
///
/// We try, in order:
/// 1. The exact PT_INTERP path (Linux semantics — a full-distro rootfs like the
///    final-2026 Debian image ships the matching loader at the absolute path,
///    and mixing a sub-tree loader with the distro libc breaks GLIBC_PRIVATE).
/// 2. Hard-coded musl shortcut (`musl/lib/libc.so` is musl's combined libc + ld).
/// 3. Hard-coded glibc loader candidates under `glibc/lib` and `glibc/lib64`.
/// 4. The original interp with leading slash stripped (root-relative).
/// 5. Same path prefixed with `glibc/` and `musl/` (sub-tree fallback).
fn map_interp_candidates(interp: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |c: String, candidates: &mut Vec<String>| {
        if !candidates.iter().any(|x| x == &c) {
            candidates.push(c);
        }
    };

    let stripped: &str = if interp.starts_with('/') { &interp[1..] } else { interp };

    if crate::fs::lookup(interp).is_ok() {
        push(String::from(interp), &mut candidates);
    }

    if interp.contains("ld-musl") {
        push(String::from("musl/lib/libc.so"), &mut candidates);
    }

    if interp.contains("ld-linux") {
        let mut g_lib = String::from("glibc/");
        g_lib.push_str(stripped);
        push(g_lib, &mut candidates);

        if interp.contains("/lib/") {
            let mut g_lib64 = String::from("glibc/");
            g_lib64.push_str(&stripped.replacen("lib/", "lib64/", 1));
            push(g_lib64, &mut candidates);
        }
        if interp.contains("/lib64/") {
            let mut g_lib = String::from("glibc/");
            g_lib.push_str(&stripped.replacen("lib64/", "lib/", 1));
            push(g_lib, &mut candidates);
        }
    }

    {
        let mut s = String::from("glibc/");
        s.push_str(stripped);
        push(s, &mut candidates);
    }
    {
        let mut s = String::from("musl/");
        s.push_str(stripped);
        push(s, &mut candidates);
    }

    push(String::from(stripped), &mut candidates);
    push(String::from(interp), &mut candidates);

    candidates
}

pub(super) fn do_exec_from_fs_path(path: &str, name: &str, argv: &[String], envp: &[String]) -> Result<ElfInfo, ExecError> {
    // Resolve the executable inode number for the WRITE-direction ETXTBSY
    // check (open-for-write fds block execve). This direction stays keyed by
    // inode because the fd-close release site only knows the inode. The
    // exec-deny direction below is keyed by absolute path (globally unique).
    let exec_ino = crate::fs::lookup(path)
        .ok()
        .map(|inode| inode.ino())
        .unwrap_or(0);

    // ETXTBSY: refuse to exec a file that has open-for-write fds (execve04).
    // Linux checks get_write_access() → ETXTBSY in do_open_execat().
    if exec_ino != 0 && crate::fs::has_write_access(exec_ino) {
        return Err(ExecError::TextBusy);
    }

    // fanotify FAN_OPEN_PERM / FAN_OPEN_EXEC_PERM: Linux do_open_execat()
    // opens the binary, so an exec must synchronously pass any permission
    // monitor before the image is loaded. DENY fails the execve with EPERM
    // while the old image is still intact (fanotify03). The wait blocks via
    // syscall restart, so it must precede any irreversible state change.
    if crate::fs::notify::fanotify::has_active_marks() {
        if let Err(_errno) = crate::fs::notify::fanotify::notify_perm_path(
            path,
            crate::fs::notify::fanotify::FAN_OPEN_PERM
                | crate::fs::notify::fanotify::FAN_OPEN_EXEC_PERM,
        ) {
            return Err(ExecError::PermissionDenied);
        }
    }

    let mut new_address_space = AddressSpace::new();
    let mut elf_info = ElfLoader::load_from_fs(path, &mut new_address_space)
        .map_err(|e| ExecError::LoadFailed(e))?;

    if let Some(ref interp_path) = elf_info.interp_path {
        let candidates = map_interp_candidates(interp_path);
        let mut loaded = false;
        let mut last_err: Option<crate::loader::LoaderError> = None;

        for candidate in &candidates {
            match ElfLoader::load_interp_from_fs(candidate, &mut new_address_space) {
                Ok((interp_entry, interp_base)) => {
                    elf_info.app_entry = elf_info.entry;
                    elf_info.interp_base = interp_base;
                    elf_info.entry = interp_entry;
                    loaded = true;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if !matches!(e, crate::loader::LoaderError::FileNotFound) {
                        crate::println!(
                            "[exec] WARN: interp load non-FNF error: {} -> {}: {:?}",
                            interp_path, candidate, e
                        );
                    }
                }
            }
        }

        if !loaded {
            crate::println!(
                "[exec] interp not found for '{}' after trying {} candidates; last_err={:?}",
                interp_path,
                candidates.len(),
                last_err
            );
            return Err(ExecError::LoadFailed(
                last_err.unwrap_or(crate::loader::LoaderError::FileNotFound),
            ));
        }
    }

    allocate_user_stack(&mut new_address_space, elf_info.stack_top)?;
    let (new_sp, saved_auxv) =
        push_args_to_stack(&mut new_address_space, elf_info.stack_top, argv, envp, &elf_info)?;
    elf_info.stack_top = new_sp;

    // Release old exec write-deny before replacing the address space.
    // The path this process is responsible for releasing is stored in the PCB.
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_mut(pid) {
                    if !process.exec_deny_path.is_empty() {
                        let old_path = core::mem::take(&mut process.exec_deny_path);
                        crate::fs::allow_write_access(&old_path);
                    }
                }
            }
        }
    }

    replace_current_process_address_space(
        new_address_space,
        &elf_info,
        name,
        path,
        argv,
        envp,
        saved_auxv,
    )?;

    // fanotify FAN_OPEN_EXEC: a successful execve opens the binary for
    // execution. Linux delivers FAN_OPEN and FAN_OPEN_EXEC together at
    // open_exec() time; report both bits so inode/mount/fs marks watching
    // either event fire (fanotify12). Marks not requesting these bits are
    // filtered in deliver_merged().
    crate::fs::notify::fanotify::notify_file_path(
        path,
        crate::fs::notify::fanotify::FAN_OPEN | crate::fs::notify::fanotify::FAN_OPEN_EXEC,
    );

    // After successful exec, deny write access to the new executable path
    // and record it in the PCB for cleanup on exit / re-exec.
    if exec_ino != 0 {
        crate::fs::deny_write_access(path);
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_mut(pid) {
                    process.exec_deny_path = String::from(path);
                }
            }
        }
    }

    Ok(elf_info)
}

pub(super) fn do_exec(elf_data: &[u8], name: &str, argv: &[String], envp: &[String]) -> Result<ElfInfo, ExecError> {
    let _header = ElfLoader::validate(elf_data)
        .map_err(|e| ExecError::InvalidElf(e))?;

    let mut new_address_space = AddressSpace::new();

    let mut elf_info = ElfLoader::load(elf_data, &mut new_address_space)
        .map_err(|e| ExecError::LoadFailed(e))?;

    allocate_user_stack(&mut new_address_space, elf_info.stack_top)?;

    let (new_sp, saved_auxv) =
        push_args_to_stack(&mut new_address_space, elf_info.stack_top, argv, envp, &elf_info)?;
    elf_info.stack_top = new_sp;

    replace_current_process_address_space(
        new_address_space,
        &elf_info,
        name,
        name,
        argv,
        envp,
        saved_auxv,
    )?;

    Ok(elf_info)
}
