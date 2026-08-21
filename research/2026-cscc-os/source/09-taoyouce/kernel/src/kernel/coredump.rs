//! Core dump file generation.
//!
//! Per Linux `fs/coredump.c` + `fs/binfmt_elf.c`: when a process is killed by
//! a core-dumping signal (SIGQUIT, SIGILL, SIGABRT, SIGBUS, SIGFPE, SIGSEGV,
//! SIGXCPU, SIGXFSZ, SIGSYS, SIGTRAP), the kernel writes an ELF core file
//! containing the process register state and memory layout.
//!
//! The core file path follows `/proc/sys/kernel/core_pattern` (default "core"),
//! resolved relative to the process's cwd.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sched::process::PROCESS_TABLE;

// ELF64 constants
const ELFMAG: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;
const ELFOSABI_NONE: u8 = 0;
const ET_CORE: u16 = 4;
#[cfg(target_arch = "riscv64")]
const EM_RISCV: u16 = 243;
#[cfg(target_arch = "loongarch64")]
const EM_LOONGARCH: u16 = 258;
const PT_NOTE: u32 = 4;
const PT_LOAD: u32 = 1;
const PF_R: u32 = 4;
const PF_W: u32 = 2;
const PF_X: u32 = 1;

// ELF note types
const NT_PRSTATUS: u32 = 1;
const NT_PRPSINFO: u32 = 3;

/// ELF64 file header (64 bytes)
#[repr(C)]
struct Elf64Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

/// ELF64 program header (56 bytes)
#[repr(C)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

/// Collected process snapshot for core dump generation.
/// Gathered while holding PROCESS_TABLE lock, consumed after releasing it.
struct CoreDumpInfo {
    pid: usize,
    ppid: usize,
    uid: u32,
    gid: u32,
    comm: String,
    cwd: String,
    signal: i32,
    regs: [u64; 32],
    pc: u64,
    /// Dumpable memory segments: contiguous runs of resident pages from
    /// readable, non-`MADV_DONTDUMP`, anonymous VMAs (mirroring the default
    /// `coredump_filter = 0x33`: anon private/shared dumped, file-backed and
    /// `VM_DONTDUMP` excluded). Each is written verbatim into the core file so
    /// a debugger — or LTP `madvise08`'s byte scanner — sees the live contents.
    segments: Vec<DumpSegment>,
}

struct DumpSegment {
    vaddr: usize,
    flags: u32,
    data: Vec<u8>,
}

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

fn push_le16(buf: &mut Vec<u8>, v: u16) { buf.extend_from_slice(&v.to_le_bytes()); }
fn push_le32(buf: &mut Vec<u8>, v: u32) { buf.extend_from_slice(&v.to_le_bytes()); }
fn push_le64(buf: &mut Vec<u8>, v: u64) { buf.extend_from_slice(&v.to_le_bytes()); }

/// Build the ELF note for NT_PRSTATUS (register state).
/// Per Linux `include/uapi/linux/elfcore.h` struct elf_prstatus for RISC-V:
///   siginfo (128 bytes) + general fields + pr_reg (32 GP regs + pc = 33 * 8 = 264 bytes)
fn build_prstatus_note(info: &CoreDumpInfo) -> Vec<u8> {
    let mut note = Vec::new();
    // Note header: namesz=5 ("CORE\0"), descsz, type=NT_PRSTATUS
    let name = b"CORE\0";
    let namesz = name.len() as u32;
    // prstatus for riscv64: struct elf_prstatus is ~320+ bytes
    // We use a simplified but valid layout:
    //   si_signo(4) + si_code(4) + si_errno(4) + pr_cursig(2) + pad(2) +
    //   pr_sigpend(8) + pr_sighold(8) + pr_pid(4) + pr_ppid(4) +
    //   pr_pgrp(4) + pr_sid(4) + pr_utime(16) + pr_stime(16) + pr_cutime(16) + pr_cstime(16) +
    //   pr_reg(33*8=264) + pr_fpvalid(4) + pad(4)
    // Total desc size = 4+4+4+2+2+8+8+4+4+4+4+16+16+16+16+264+4+4 = 384
    let descsz: u32 = 384;

    push_le32(&mut note, namesz);
    push_le32(&mut note, descsz);
    push_le32(&mut note, NT_PRSTATUS);
    note.extend_from_slice(name);
    // Pad name to 4-byte alignment
    let name_pad = align_up(name.len(), 4) - name.len();
    for _ in 0..name_pad { note.push(0); }

    // si_signo, si_code, si_errno
    push_le32(&mut note, info.signal as u32);
    push_le32(&mut note, 0); // si_code
    push_le32(&mut note, 0); // si_errno
    // pr_cursig (u16) + pad (u16)
    push_le16(&mut note, info.signal as u16);
    push_le16(&mut note, 0);
    // pr_sigpend, pr_sighold
    push_le64(&mut note, 0);
    push_le64(&mut note, 0);
    // pr_pid, pr_ppid, pr_pgrp, pr_sid
    push_le32(&mut note, info.pid as u32);
    push_le32(&mut note, info.ppid as u32);
    push_le32(&mut note, info.pid as u32); // pgrp = pid
    push_le32(&mut note, info.pid as u32); // sid = pid
    // pr_utime (struct timeval: 2 * u64)
    push_le64(&mut note, 0); push_le64(&mut note, 0);
    // pr_stime
    push_le64(&mut note, 0); push_le64(&mut note, 0);
    // pr_cutime
    push_le64(&mut note, 0); push_le64(&mut note, 0);
    // pr_cstime
    push_le64(&mut note, 0); push_le64(&mut note, 0);
    // pr_reg: 32 GP regs + pc (RISC-V: x0-x31 + pc)
    for i in 0..32 {
        push_le64(&mut note, info.regs[i]);
    }
    push_le64(&mut note, info.pc); // sepc = program counter
    // pr_fpvalid + pad
    push_le32(&mut note, 0);
    push_le32(&mut note, 0);

    // Pad desc to 4-byte alignment (384 is already aligned)
    note
}

/// Build the ELF note for NT_PRPSINFO (process info).
/// Per Linux `include/uapi/linux/elfcore.h` struct elf_prpsinfo.
fn build_prpsinfo_note(info: &CoreDumpInfo) -> Vec<u8> {
    let mut note = Vec::new();
    let name = b"CORE\0";
    let namesz = name.len() as u32;
    // struct elf_prpsinfo (136 bytes for 64-bit):
    //   pr_state(1) + pr_sname(1) + pr_zomb(1) + pr_nice(1) +
    //   pad(4) + pr_flag(8) + pr_uid(4) + pr_gid(4) +
    //   pr_pid(4) + pr_ppid(4) + pr_pgrp(4) + pr_sid(4) +
    //   pr_fname(16) + pr_psargs(80)
    let descsz: u32 = 136;

    push_le32(&mut note, namesz);
    push_le32(&mut note, descsz);
    push_le32(&mut note, NT_PRPSINFO);
    note.extend_from_slice(name);
    let name_pad = align_up(name.len(), 4) - name.len();
    for _ in 0..name_pad { note.push(0); }

    // pr_state=0 (R), pr_sname='R', pr_zomb=0, pr_nice=0
    note.push(0);
    note.push(b'R');
    note.push(0);
    note.push(0);
    // pad(4) + pr_flag(8)
    push_le32(&mut note, 0);
    push_le64(&mut note, 0);
    // pr_uid, pr_gid
    push_le32(&mut note, info.uid);
    push_le32(&mut note, info.gid);
    // pr_pid, pr_ppid, pr_pgrp, pr_sid
    push_le32(&mut note, info.pid as u32);
    push_le32(&mut note, info.ppid as u32);
    push_le32(&mut note, info.pid as u32);
    push_le32(&mut note, info.pid as u32);
    // pr_fname (16 bytes, null-padded)
    let mut fname = [0u8; 16];
    let comm_bytes = info.comm.as_bytes();
    let copy_len = comm_bytes.len().min(15);
    fname[..copy_len].copy_from_slice(&comm_bytes[..copy_len]);
    note.extend_from_slice(&fname);
    // pr_psargs (80 bytes, null-padded)
    let mut psargs = [0u8; 80];
    psargs[..copy_len].copy_from_slice(&comm_bytes[..copy_len]);
    note.extend_from_slice(&psargs);

    note
}

/// Generate a complete ELF64 core dump file as a byte vector.
fn generate_elf_core(info: &CoreDumpInfo) -> Vec<u8> {
    let prstatus_note = build_prstatus_note(info);
    let prpsinfo_note = build_prpsinfo_note(info);
    let notes_total = prstatus_note.len() + prpsinfo_note.len();

    // Layout: ELF header | PT_NOTE phdr | PT_LOAD phdrs... | notes data |
    //         segment contents...
    let ehdr_size = 64usize;
    let phdr_size = 56usize;
    // 1 PT_NOTE + one PT_LOAD per dumpable memory segment, this time with the
    // segment contents actually written out (Linux `fs/binfmt_elf.c`
    // `elf_core_dump`), so a debugger / LTP `madvise08` can read the bytes.
    let num_phdrs = 1 + info.segments.len();
    let phdrs_offset = ehdr_size;
    let notes_offset = align_up(phdrs_offset + phdr_size * num_phdrs, 8);
    let data_offset = align_up(notes_offset + notes_total, 0x1000);
    // Precompute each segment's file offset.
    let mut seg_offsets = Vec::with_capacity(info.segments.len());
    let mut cur = data_offset;
    for seg in &info.segments {
        seg_offsets.push(cur);
        cur += seg.data.len();
    }
    let file_size = cur;

    let mut buf = Vec::with_capacity(file_size);

    // === ELF header ===
    buf.extend_from_slice(&ELFMAG);
    buf.push(ELFCLASS64);
    buf.push(ELFDATA2LSB);
    buf.push(EV_CURRENT);
    buf.push(ELFOSABI_NONE);
    buf.extend_from_slice(&[0u8; 8]); // e_ident padding
    push_le16(&mut buf, ET_CORE);
    #[cfg(target_arch = "riscv64")]
    push_le16(&mut buf, EM_RISCV);
    #[cfg(target_arch = "loongarch64")]
    push_le16(&mut buf, EM_LOONGARCH);
    push_le32(&mut buf, 1); // e_version
    push_le64(&mut buf, 0); // e_entry
    push_le64(&mut buf, phdrs_offset as u64); // e_phoff
    push_le64(&mut buf, 0); // e_shoff
    push_le32(&mut buf, 0); // e_flags
    push_le16(&mut buf, ehdr_size as u16); // e_ehsize
    push_le16(&mut buf, phdr_size as u16); // e_phentsize
    push_le16(&mut buf, num_phdrs as u16); // e_phnum
    push_le16(&mut buf, 0); // e_shentsize
    push_le16(&mut buf, 0); // e_shnum
    push_le16(&mut buf, 0); // e_shstrndx

    // === Program headers ===
    // PT_NOTE
    push_le32(&mut buf, PT_NOTE);
    push_le32(&mut buf, 0); // p_flags
    push_le64(&mut buf, notes_offset as u64); // p_offset
    push_le64(&mut buf, 0); // p_vaddr
    push_le64(&mut buf, 0); // p_paddr
    push_le64(&mut buf, notes_total as u64); // p_filesz
    push_le64(&mut buf, notes_total as u64); // p_memsz
    push_le64(&mut buf, 4); // p_align

    // PT_LOAD entries (one per dumpable segment, contents written below).
    for (seg, &off) in info.segments.iter().zip(seg_offsets.iter()) {
        let sz = seg.data.len() as u64;
        push_le32(&mut buf, PT_LOAD);
        push_le32(&mut buf, seg.flags);
        push_le64(&mut buf, off as u64); // p_offset
        push_le64(&mut buf, seg.vaddr as u64); // p_vaddr
        push_le64(&mut buf, 0); // p_paddr
        push_le64(&mut buf, sz); // p_filesz (memory dumped)
        push_le64(&mut buf, sz); // p_memsz
        push_le64(&mut buf, 0x1000); // p_align (page size)
    }

    // Pad to notes_offset
    while buf.len() < notes_offset {
        buf.push(0);
    }

    // === Notes ===
    buf.extend_from_slice(&prstatus_note);
    buf.extend_from_slice(&prpsinfo_note);

    // Pad to the first segment's file offset.
    while buf.len() < data_offset {
        buf.push(0);
    }

    // === Segment contents ===
    for seg in &info.segments {
        buf.extend_from_slice(&seg.data);
    }

    buf
}

/// Generate a core dump for the current process being killed by `signal`.
/// Called from the signal delivery path before process exit.
/// Per Linux `fs/coredump.c do_coredump()`.
pub fn do_coredump(signal: i32) {
    // Gather process info while holding the process table lock
    let info = {
        let table = PROCESS_TABLE.lock();
        let table = match table.as_ref() {
            Some(t) => t,
            None => return,
        };
        let pid = match table.current {
            Some(pid) => pid,
            None => return,
        };
        let process = match table.get(pid) {
            Some(p) => p,
            None => return,
        };

        // Check dumpable flag (per Linux `get_dumpable()`)
        if process.dumpable == 0 {
            return;
        }

        // RLIMIT_CORE（idx 4）：软限为 core 文件字节上限。Linux 默认 0 →
        // 不产生 core。limit==0 直接跳过（crashme 等故意触发 SIGSEGV/SIGILL
        // 的用例不应产生 core；否则把整个进程的常驻匿名内存拷进内核 Vec
        // 会撑爆内核堆）。limit>0 时把 dump 总字节截断到该上限。
        let core_limit = process.rlimits[4].0;
        if core_limit == 0 {
            return;
        }

        let ppid = process.parent_pid.unwrap_or(0);
        let uid = process.uid;
        let gid = process.gid;
        let comm = process.comm.clone();
        let cwd = process.cwd();

        // Collect register state from trap context
        let (regs, pc) = unsafe {
            if let Some(ctx) = process.trap_context() {
                let mut regs = [0u64; 32];
                for i in 0..32 {
                    regs[i] = ctx.x[i] as u64;
                }
                (regs, ctx.sepc as u64)
            } else {
                ([0u64; 32], 0u64)
            }
        };

        // Collect dumpable memory segments. Mirroring the default
        // `coredump_filter = 0x33`: dump anonymous (non-file-backed) readable
        // VMAs, skip `MADV_DONTDUMP` ranges. Within each VMA we emit one
        // segment per contiguous run of *resident* pages (so a sparsely
        // touched stack/heap doesn't balloon the dump), copying the live
        // bytes out of the page table.
        let segments: Vec<DumpSegment> = if let Some(ref mm) = process.mm {
            use crate::mm::{VirtPageNum, PAGE_SIZE};
            let addr_space = mm.lock();
            let mut segs: Vec<DumpSegment> = Vec::new();
            // 已累计的 dump 字节，受 RLIMIT_CORE 截断（防内核堆撑爆）。
            let cap = core_limit as usize;
            let mut total: usize = 0;
            'vma: for vma in addr_space.vmas_iter() {
                if total >= cap { break; }
                if !vma.permission.read || vma.dontdump || vma.file_backed {
                    continue;
                }
                let mut flags: u32 = 0;
                if vma.permission.read { flags |= PF_R; }
                if vma.permission.write { flags |= PF_W; }
                if vma.permission.execute { flags |= PF_X; }
                let start_page = vma.start.0 & !(PAGE_SIZE - 1);
                let end = vma.end.0;
                let mut run: Option<(usize, Vec<u8>)> = None;
                let mut va = start_page;
                while va < end {
                    let vpn = VirtPageNum(va >> 12);
                    let phys = addr_space.page_table().translate(vpn);
                    match phys {
                        Some(ppn) => {
                            let kptr = crate::mm::phys_to_kernel_vaddr(ppn.0 << 12) as *const u8;
                            let n = core::cmp::min(PAGE_SIZE, end - va);
                            let page = unsafe { core::slice::from_raw_parts(kptr, n) };
                            match run.as_mut() {
                                Some((_, data)) => data.extend_from_slice(page),
                                None => {
                                    let mut data = Vec::with_capacity(n);
                                    data.extend_from_slice(page);
                                    run = Some((va, data));
                                }
                            }
                            total += n;
                            if total >= cap {
                                if let Some((vaddr, data)) = run.take() {
                                    segs.push(DumpSegment { vaddr, flags, data });
                                }
                                break 'vma;
                            }
                        }
                        None => {
                            if let Some((vaddr, data)) = run.take() {
                                segs.push(DumpSegment { vaddr, flags, data });
                            }
                        }
                    }
                    va += PAGE_SIZE;
                }
                if let Some((vaddr, data)) = run.take() {
                    segs.push(DumpSegment { vaddr, flags, data });
                }
            }
            segs
        } else {
            Vec::new()
        };

        CoreDumpInfo {
            pid,
            ppid,
            uid,
            gid,
            comm,
            cwd,
            signal,
            regs,
            pc,
            segments,
        }
    };
    // PROCESS_TABLE lock released here

    // Generate ELF core file
    let core_data = generate_elf_core(&info);

    // Resolve the core-file path from `/proc/sys/kernel/core_pattern`,
    // expanding the format specifiers Linux supports (we implement the common
    // ones used by LTP: %p pid, %u uid, %g gid, %s signal, %e/%E/%c comm-ish,
    // %% literal). A relative pattern is resolved against the process cwd.
    let pattern = crate::fs::proc::sys::core_pattern();
    let expanded = expand_core_pattern(&pattern, &info);
    let core_path = if expanded.starts_with('/') {
        expanded
    } else if info.cwd == "/" {
        let mut p = String::from("/");
        p.push_str(&expanded);
        p
    } else {
        let mut p = info.cwd.clone();
        if !p.ends_with('/') {
            p.push('/');
        }
        p.push_str(&expanded);
        p
    };

    // Create parent dirs if needed and write the file
    if crate::fs::create_file_if_missing(&core_path, crate::fs::FileType::Regular).is_ok() {
        if let Ok(inode) = crate::fs::lookup(&core_path) {
            if let Some(file) = inode.as_file() {
                // Truncate existing content by writing from offset 0
                let _ = file.write(0, &core_data);
            }
        }
    }
}

/// Expand a `core_pattern` template into a file name. Implements the common
/// Linux `%`-specifiers; an unknown specifier is dropped (as Linux does), and a
/// trailing lone `%` is ignored. `%%` yields a literal `%`.
fn expand_core_pattern(pattern: &str, info: &CoreDumpInfo) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('p') => out.push_str(&alloc::format!("{}", info.pid)),
            Some('P') => out.push_str(&alloc::format!("{}", info.pid)),
            Some('u') => out.push_str(&alloc::format!("{}", info.uid)),
            Some('g') => out.push_str(&alloc::format!("{}", info.gid)),
            Some('s') => out.push_str(&alloc::format!("{}", info.signal)),
            Some('e') | Some('E') | Some('c') => out.push_str(&info.comm),
            // Drop unsupported specifiers (e.g. %t %h %d) rather than emit junk.
            Some(_) => {}
            None => {}
        }
    }
    out
}
