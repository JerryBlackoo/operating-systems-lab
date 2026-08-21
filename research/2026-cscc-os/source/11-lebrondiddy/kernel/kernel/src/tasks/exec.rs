use super::{stack::init_task_stack, UserTask};
use crate::{
    consts::USER_DYN_ADDR,
    tasks::{elf::ElfExtra, MemType},
};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    ops::Mul,
    sync::atomic::{AtomicU8, Ordering},
};
use devices::{frame_alloc_much, PAGE_SIZE};
use fs::{file::File, pathbuf::PathBuf};
use libc_types::fcntl::OpenFlags;
use syscalls::Errno;
use xmas_elf::program::{SegmentData, Type};

static LOADER_TRACE_STATE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn loader_trace_enabled() -> bool {
    match LOADER_TRACE_STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let enabled = File::open("/.loader_trace", OpenFlags::RDONLY).is_ok();
            LOADER_TRACE_STATE.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
            enabled
        }
    }
}

pub(crate) fn is_loader_trace_path(path: &str) -> bool {
    path.contains("ld-linux")
        || path.ends_with("libc.so")
        || path.ends_with("libc.so.6")
        || path.ends_with("libm.so")
        || path.ends_with("libm.so.6")
        || path.ends_with("libpthread.so.0")
        || path.ends_with("librt.so.1")
        || path.ends_with("libdl.so.2")
        || path.ends_with("libresolv.so.2")
}

fn interp_path(data: &[u8]) -> Option<String> {
    let len = data.iter().position(|x| *x == 0).unwrap_or(data.len());
    core::str::from_utf8(&data[..len])
        .ok()
        .filter(|path| !path.is_empty())
        .map(|path| path.to_string())
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn push_candidate(candidates: &mut Vec<String>, candidate: String) {
    if !candidate.is_empty() && candidates.iter().all(|existing| existing != &candidate) {
        candidates.push(candidate);
    }
}

fn resolve_interpreter(curr_dir: &PathBuf, requested: &str) -> Option<String> {
    let base = basename(requested);
    let mut candidates = Vec::new();

    if curr_dir.path() == "/glibc" || curr_dir.path().starts_with("/glibc/") {
        push_candidate(&mut candidates, format!("/glibc/lib/{}", base));
    } else if curr_dir.path() == "/musl" || curr_dir.path().starts_with("/musl/") {
        push_candidate(&mut candidates, format!("/musl/lib/{}", base));
        push_candidate(&mut candidates, "/musl/lib/libc.so".to_string());
    }
    push_candidate(&mut candidates, requested.to_string());
    push_candidate(&mut candidates, base.to_string());
    push_candidate(&mut candidates, format!("lib/{}", base));
    push_candidate(&mut candidates, "libc.so".to_string());
    push_candidate(&mut candidates, "lib/libc.so".to_string());

    candidates.into_iter().find(|candidate| {
        let resolved = curr_dir.join(candidate.as_str()).path();
        let found = File::open_link(resolved.as_str(), OpenFlags::RDONLY).is_ok();
        if loader_trace_enabled() && (found || is_loader_trace_path(resolved.as_str())) {
            println!(
                "loader-trace: interp candidate requested={} cwd={} path={} result={}",
                requested,
                curr_dir.path(),
                resolved,
                if found { "ok" } else { "err" }
            );
        }
        found
    })
}

fn env_value<'a>(envp: &'a [String], key: &str) -> Option<&'a str> {
    let prefix_len = key.len() + 1;
    envp.iter()
        .find(|entry| entry.starts_with(key) && entry.as_bytes().get(key.len()) == Some(&b'='))
        .map(|entry| &entry[prefix_len..])
}

fn resolve_program(curr_dir: &PathBuf, requested: &str, envp: &[String]) -> Option<String> {
    if requested == "/proc/self/exe" {
        return Some(requested.to_string());
    }

    if requested.starts_with('/') || requested.contains('/') {
        return Some(requested.to_string());
    }

    if File::open_link(curr_dir.join(requested), OpenFlags::RDONLY).is_ok() {
        return Some(requested.to_string());
    }

    if let Some(path) = env_value(envp, "PATH") {
        for dir in path.split(':') {
            let candidate = if dir.is_empty() {
                requested.to_string()
            } else {
                format!("{}/{}", dir, requested)
            };
            if File::open_link(curr_dir.join(candidate.as_str()), OpenFlags::RDONLY).is_ok() {
                return Some(candidate);
            }
        }
    }

    None
}

fn resolve_busybox(curr_dir: &PathBuf) -> Option<String> {
    [
        "busybox",
        "../busybox",
        "/busybox",
        "/glibc/busybox",
        "/musl/busybox",
    ]
    .into_iter()
    .find(|candidate| File::open_link(curr_dir.join(candidate), OpenFlags::RDONLY).is_ok())
    .map(|candidate| candidate.to_string())
}

fn busybox_applet(path: &str) -> Option<&str> {
    let name = basename(path);
    const APPLETS: [&str; 42] = [
        "[", "ash", "basename", "cal", "cat", "clear", "cp", "cut", "date", "df", "dirname",
        "dmesg", "du", "echo", "expr", "false", "find", "free", "grep", "head", "hexdump",
        "hwclock", "kill", "ls", "md5sum", "mkdir", "more", "mv", "od", "printf", "ps", "pwd",
        "rm", "rmdir", "sh", "sleep", "sort", "stat", "strings", "tail", "true", "wc",
    ];
    APPLETS.iter().any(|applet| *applet == name).then_some(name)
}

fn close_cloexec_fds(task: &UserTask) {
    let mut pcb = task.pcb.lock();
    for fd in pcb.fd_table.iter_mut() {
        let close_on_exec = fd
            .as_ref()
            .is_some_and(|file| file.flags.lock().contains(OpenFlags::CLOEXEC));
        if close_on_exec {
            *fd = None;
        }
    }
}

pub fn exec_with_process(
    task: Arc<UserTask>,
    curr_dir: PathBuf,
    path: String,
    args: Vec<String>,
    envp: Vec<String>,
) -> Result<Arc<UserTask>, Errno> {
    // copy args, avoid free before pushing.
    // let path = String::from(path);
    let exec_path = path;
    let resolved_path = resolve_program(&curr_dir, &exec_path, &envp);
    let path = curr_dir.join(resolved_path.as_ref().unwrap_or(&exec_path));
    let path_string = path.path();
    if loader_trace_enabled() && is_loader_trace_path(path_string.as_str()) {
        println!(
            "loader-trace: kernel exec cwd={} requested={} resolved={}",
            curr_dir.path(),
            exec_path,
            path_string
        );
    }

    let user_task = task.clone();
    let self_exe_path = user_task.pcb.lock().exe_path.clone();

    // TODO: 运行程序的时候，判断当前的路径
    let path = if exec_path == "/proc/self/exe" {
        PathBuf::from(self_exe_path)
    } else {
        path
    };
    let file = match File::open_link(path.clone(), OpenFlags::RDONLY) {
        Ok(file) => Arc::new(file).clone(),
        Err(err) => {
            if let Some(applet) = busybox_applet(&exec_path) {
                let Some(busybox) = resolve_busybox(&curr_dir) else {
                    return Err(err);
                };
                let mut new_args = vec![busybox.clone(), applet.to_string()];
                new_args.extend(args.into_iter().skip(1));
                return exec_with_process(task, curr_dir, busybox, new_args, envp);
            }
            return Err(err);
        }
    };
    let file_size = file.file_size()?;
    if file_size == 0 {
        return Err(Errno::ENOEXEC);
    }
    let frame_ppn = frame_alloc_much(file_size.div_ceil(PAGE_SIZE)).ok_or(Errno::ENOMEM)?;
    let buffer = frame_ppn
        .first()
        .ok_or(Errno::ENOMEM)?
        .slice_mut_with_len(file_size);
    let rsize = file.readat(0, buffer)?;
    if rsize != file_size {
        return Err(Errno::ENOEXEC);
    }
    // flush_dcache_range();
    // 读取elf信息
    let elf = if let Ok(elf) = xmas_elf::ElfFile::new(&buffer) {
        elf
    } else {
        let shell = resolve_busybox(&curr_dir).unwrap_or_else(|| "/busybox".to_string());
        let mut new_args = vec![shell.to_string(), "sh".to_string()];
        new_args.push(path_string);
        args.iter().skip(1).for_each(|x| new_args.push(x.clone()));
        return exec_with_process(task, curr_dir, shell, new_args, envp);
    };
    let elf_header = elf.header;

    let entry_point = elf.header.pt2.entry_point() as usize;
    // this assert ensures that the file is elf file.
    if elf_header.pt1.magic != [0x7f, 0x45, 0x4c, 0x46] {
        return Err(Errno::ENOEXEC);
    }
    // WARRNING: this convert async task to user task.
    let user_task = task.clone();

    // check if it is libc, dlopen, it needs recurit.
    let header = elf
        .program_iter()
        .find(|ph| ph.get_type() == Ok(Type::Interp));
    if let Some(header) = header {
        if let Ok(SegmentData::Undefined(_data)) = header.get_data(&elf) {
            let Some(interp) = interp_path(_data) else {
                return Err(Errno::ENOEXEC);
            };
            let program_path = path.path();
            let Some(loader) = resolve_interpreter(&path.dir(), &interp) else {
                println!("dynamic interpreter not found: {}", interp);
                return Err(Errno::ENOENT);
            };
            if loader_trace_enabled() {
                println!(
                    "loader-trace: PT_INTERP program={} cwd={} interp={} loader={}",
                    program_path,
                    curr_dir.path(),
                    interp,
                    curr_dir.join(loader.as_str()).path()
                );
            }
            drop(frame_ppn);
            let mut new_args = vec![loader.clone(), program_path];
            new_args.extend(args.into_iter().skip(1));
            return exec_with_process(task, curr_dir, loader, new_args, envp);
        }
    }

    let base = elf.relocate(USER_DYN_ADDR).unwrap_or(0);

    // Validate loadable segments before replacing the old address space.
    let mut heap_top: Option<usize> = None;
    let mut load_segments = Vec::new();
    for ph in elf.program_iter() {
        let end = ph
            .virtual_addr()
            .checked_add(ph.mem_size())
            .ok_or(Errno::ENOEXEC)? as usize;
        heap_top = Some(heap_top.map_or(end, |top| top.max(end)));

        if ph.get_type().map_err(|_| Errno::ENOEXEC)? != xmas_elf::program::Type::Load {
            continue;
        }

        let file_size = ph.file_size() as usize;
        let mem_size = ph.mem_size() as usize;
        if file_size > mem_size {
            return Err(Errno::ENOEXEC);
        }
        if mem_size == 0 {
            continue;
        }

        let offset = ph.offset() as usize;
        let offset_end = offset.checked_add(file_size).ok_or(Errno::ENOEXEC)?;
        if offset_end > buffer.len() {
            return Err(Errno::ENOEXEC);
        }

        let virt_addr = base
            .checked_add(ph.virtual_addr() as usize)
            .ok_or(Errno::ENOEXEC)?;
        let virt_end = virt_addr.checked_add(mem_size).ok_or(Errno::ENOEXEC)?;
        let vpn = virt_addr / PAGE_SIZE;
        let page_count = virt_end
            .div_ceil(PAGE_SIZE)
            .checked_sub(vpn)
            .filter(|count| *count > 0)
            .ok_or(Errno::ENOEXEC)?;

        load_segments.push((
            file_size, mem_size, offset, offset_end, virt_addr, page_count,
        ));
    }
    if load_segments.is_empty() {
        return Err(Errno::ENOEXEC);
    }

    // 获取程序所有段之后的内存，4K 对齐后作为堆底
    let heap_bottom = heap_top
        .ok_or(Errno::ENOEXEC)?
        .div_ceil(PAGE_SIZE)
        .mul(PAGE_SIZE);
    close_cloexec_fds(&user_task);
    user_task.pcb.lock().memset.clear();
    user_task.page_table.restore();
    user_task.page_table.change();

    init_task_stack(
        user_task.clone(),
        args,
        envp,
        base,
        &path.path(),
        entry_point,
        elf_header.pt2.ph_count() as usize,
        elf_header.pt2.ph_entry_size() as usize,
        elf.get_ph_addr().unwrap_or(0) as usize,
        heap_bottom,
    );

    // map sections.
    for (file_size, mem_size, offset, offset_end, virt_addr, page_count) in load_segments {
        user_task
            .frame_alloc(va!(virt_addr).floor(), MemType::CodeSection, page_count)
            .ok_or(Errno::ENOMEM)?;
        let page_space = va!(virt_addr).slice_mut_with_len(mem_size);

        page_space[..file_size].copy_from_slice(&buffer[offset..offset_end]);
        page_space[file_size..].fill(0);
    }
    Ok(user_task)
}
