//! 内核命令行解析模块
//!
//! 从 DTB (Flattened Device Tree) 中解析 `/chosen/bootargs` 属性，
//! 将内核命令行保存到全局静态缓冲区，并提供访问接口。
//!
//! # FDT 格式简述
//!
//! FDT 由三部分组成：header、structure block、strings block。
//! - Structure block 包含节点和属性的树形结构
//! - Strings block 包含所有属性名字符串
//! - 属性通过 FDT_PROP token 表示，包含 len、nameoff（指向 strings block 的偏移）
//!
//! 本模块实现了一个最小化的 FDT 解析器，仅提取 `bootargs` 属性值。

use core::sync::atomic::{AtomicUsize, Ordering};

/// 命令行最大长度
const CMDLINE_MAX: usize = 16384;

/// 全局命令行缓冲区
static mut CMDLINE_BUF: [u8; CMDLINE_MAX] = [0u8; CMDLINE_MAX];

/// 命令行实际长度
static CMDLINE_LEN: AtomicUsize = AtomicUsize::new(0);

/// 标记是否已初始化
static CMDLINE_INITIALIZED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Cached debug flags — parsed once at init, queried O(1) in hot paths.
static SIGFRDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SCDBG_SIG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static STWDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static CLDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static TICKDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static PFDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static WTRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static WTRACE_SAVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SCDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static WAITDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static WAITDUMP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// HANGDUMP=1：全内核挂死法证——上下文切换计数冻结超过阈值时，
/// 周期性把整张进程表（pid/状态/wait_channel/sepc）倒到串口。
static HANGDUMP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static FTXDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static WRDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static UDPDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static NETDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static NSDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static ITDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static FANDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static H23DBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static M9DBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static CL3DBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static HUNGDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// FSTRACE=1：路径解析失败轨迹。每次全局 `fs::lookup` 返回错误时打印 pid、
/// errno 与完整路径。用于把「cargo 说找不到某个包」直接对到「内核对哪条路径
/// 报了不存在」，无需从现象反推。命中路径零输出。
static FSTRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// FTXTRACE=1：futex 入队/唤醒轨迹。
///
/// 停摆取证已证明存在「等待字已变成 0（条件成立）、等待者仍在睡」的丢唤醒，
/// 且给出了具体地址。要判定是「唤醒方压根没发」还是「发了但没找到人」，必须
/// 同时看到同一地址上的入队与唤醒两侧事件，故按地址成对记录。
static FTXTRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static LAVMADBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// EXECDBG=1：execve(2) 入口/结果取证——打印发起 pid、解析后的路径、argv[0]
/// 与最终返回值，用于区分 exec 自身失败与 clone/spawn 阶段失败。
static EXECDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// BSCRASH=1：BuildStorm 确定性崩溃的一次性无锁 crash capsule。
static BSCRASH: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// FRAMEPROBE=1：帧引用计数取证——fork clone / CoW 复制 / 退出释放三点的
/// ppn/MetaSlot ref/Arc strong 实测值（仅诊断探针轮，限量打印）。
static FRAMEPROBE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// PTESITE=1：只开启 pte_site 安装站点环记录（无打印风暴），供 PTE-LEAK
/// 类泄漏映射溯源。EXECDBG 也会开启环，但会附带 sigdeliver 等打印风暴。
static PTESITE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// SPINDBG=1：自旋锁争用超阈值（~400 万轮）时打印持有者/请求者 RA+pid，
/// 死锁/活锁现场直接点名（2026-08-15 RV SMP=2 init 挂死取证）。
static SPINDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// VMAOPDBG=1：VMA 生命周期全量跟踪（每 mmap/munmap/split 一行）。与 LAVMADBG
/// 解耦——LAVMADBG 是崩溃取证（用户致命访存才打），VMAOP 是热路径洪流
/// （2026-08-15 长跑 170025 实测：LAVMADBG 连带 VMAOP 把 minibuild 拖慢数倍、
/// 串口被打爆），两者不得再共用一个开关。
static VMAOPDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// SEGVDBG=1：用户态致命故障（即将投递 SIGSEGV/SIGBUS/SIGILL 等）第一手取证。
/// 与 LAVMADBG 互补：LAVMADBG 只覆盖 handle_page_fault_trap 主路径且附带 VMA
/// 邻域转储，SEGVDBG 在所有 try_queue_fault_signal 站点统一打印
/// pid/ERA/BADV/cause/sp/ra（240 条预算，防嵌套信号循环淹没串口）。
static SEGVDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// BLKDBG=1：块设备读取取证——每次 VirtioBlkReader::read 打印请求的
/// start_byte/扇区/长度与返回数据前 8 字节（预算 64 条）。用于定位
/// 「文件内容错位」（r5 长跑错页根因排查：pread .so 偏移返回相邻区域内容）。
static BLKDBG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[inline(always)]
pub fn sigfrdbg_enabled() -> bool { SIGFRDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn scdbg_sig_enabled() -> bool { SCDBG_SIG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn stwdbg_enabled() -> bool { STWDBG.load(Ordering::Relaxed) }

pub fn cldbg_enabled() -> bool { CLDBG.load(Ordering::Relaxed) }

pub fn tickdbg_enabled() -> bool { TICKDBG.load(Ordering::Relaxed) }

pub fn pfdbg_enabled() -> bool { PFDBG.load(Ordering::Relaxed) }

pub fn wtrace_enabled() -> bool { WTRACE.load(Ordering::Relaxed) }

pub fn wtrace_save_enabled() -> bool { WTRACE_SAVE.load(Ordering::Relaxed) }

pub fn scdbg_enabled() -> bool { SCDBG.load(Ordering::Relaxed) }

pub fn waitdbg_enabled() -> bool { WAITDBG.load(Ordering::Relaxed) }

pub fn waitdump_enabled() -> bool { WAITDUMP.load(Ordering::Relaxed) }

pub fn hangdump_enabled() -> bool { HANGDUMP.load(Ordering::Relaxed) }

/// pipe 写端直递默认**关闭**（opt-in：PIPE_HANDOFF=1）。
/// 2026-08-15 r4 实测（dd bs=1M | cat | wc -c，LA debug，WRHDBG=1 全程零
/// register/take/once 事件）：默认开启后输出在 1835008/2031616/2097152 间
/// 摆动（重复写入），且 take 补 a0_base+off 后反而翻倍——重复源不在写端直递
/// 路径本身，而在其与 read 直递/重放时序的相互作用，需专项微基准继续二分；
/// 保持默认关闭（旧短写语义），写端直递修复代码与 WRHDBG 取证点保留。
static PIPE_HOFF: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[inline(always)]
pub fn pipe_handoff_enabled() -> bool { PIPE_HOFF.load(Ordering::Relaxed) }

#[inline(always)]
pub fn ftxdbg_enabled() -> bool { FTXDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn wrdbg_enabled() -> bool { WRDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn udpdbg_enabled() -> bool { UDPDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn netdbg_enabled() -> bool { NETDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn nsdbg_enabled() -> bool { NSDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn itdbg_enabled() -> bool { ITDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn fandbg_enabled() -> bool { FANDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn h23dbg_enabled() -> bool { H23DBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn m9dbg_enabled() -> bool { M9DBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn cl3dbg_enabled() -> bool { CL3DBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn hungdbg_enabled() -> bool { HUNGDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn fstrace_enabled() -> bool { FSTRACE.load(Ordering::Relaxed) }
#[inline(always)]
pub fn ftxtrace_enabled() -> bool { FTXTRACE.load(Ordering::Relaxed) }
#[inline(always)]
pub fn lavmadbg_enabled() -> bool { LAVMADBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn execdbg_enabled() -> bool { EXECDBG.load(Ordering::Relaxed) }

pub fn frameprobe_enabled() -> bool { FRAMEPROBE.load(Ordering::Relaxed) }

pub fn ptesite_enabled() -> bool { PTESITE.load(Ordering::Relaxed) }

pub fn spindbg_enabled() -> bool { SPINDBG.load(Ordering::Relaxed) }

pub fn vmaopdbg_enabled() -> bool { VMAOPDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn segvdbg_enabled() -> bool { SEGVDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn blkdbg_enabled() -> bool { BLKDBG.load(Ordering::Relaxed) }
#[inline(always)]
pub fn bscrash_enabled() -> bool { BSCRASH.load(Ordering::Relaxed) }

// FDT 常量
const FDT_MAGIC: u32 = 0xd00dfeed;
const FDT_BEGIN_NODE: u32 = 0x00000001;
const FDT_END_NODE: u32 = 0x00000002;
const FDT_PROP: u32 = 0x00000003;
const FDT_NOP: u32 = 0x00000004;
const FDT_END: u32 = 0x00000009;

/// FDT Header 结构
#[repr(C)]
struct FdtHeader {
    magic: u32,
    totalsize: u32,
    off_dt_struct: u32,
    off_dt_strings: u32,
    off_mem_rsvmap: u32,
    version: u32,
    last_comp_version: u32,
    boot_cpuid_phys: u32,
    size_dt_strings: u32,
    size_dt_struct: u32,
}

/// 从 DTB 地址初始化内核命令行
///
/// 解析 FDT 格式的 DTB，提取 `/chosen` 节点中的 `bootargs` 属性。
/// 如果 DTB 地址为 0（如 LoongArch64 无 DTB 的情况），命令行为空。
///
/// # Safety
/// 调用者必须确保 `dtb` 指向有效的 FDT 内存区域，且该内存已可访问
/// （即在 mm::init() 之后调用）。
pub fn init(dtb: usize) {
    init_with_fallback(dtb, None);
}

pub fn init_with_fallback(dtb: usize, fallback_cmdline: Option<usize>) {
    if CMDLINE_INITIALIZED.load(Ordering::Acquire) {
        return; // 已初始化，避免重复
    }

    let boot_info = normalize_boot_info_addr(dtb);

    // 尝试解析 DTB
    unsafe {
        // 先验证 DTB magic
        let magic_ptr = boot_info as *const u32;
        let magic = u32::from_be(core::ptr::read_unaligned(magic_ptr));

        if magic == FDT_MAGIC {
            crate::println!("[cmdline] DTB magic OK at {:#x}", boot_info);

            // 打印 FDT header 信息用于调试
            let header = &*(boot_info as *const FdtHeader);
            let struct_off = u32::from_be(header.off_dt_struct) as usize;
            let strings_off = u32::from_be(header.off_dt_strings) as usize;
            crate::println!("[cmdline] FDT struct_off={:#x} strings_off={:#x}", struct_off, strings_off);

            let cpu_count = parse_fdt_cpu_count(boot_info);
            if cpu_count > 0 {
                let bounded = cpu_count.min(crate::sched::smp::MAX_CPUS);
                crate::config::set_num_cpus(bounded);
                crate::println!("[cmdline] FDT CPU nodes found: {}, using {} CPUs", cpu_count, bounded);
            }

            if let Some((ptr, len)) = parse_fdt_bootargs(boot_info) {
                let copy_len = store_cmdline(ptr, len);
                crate::println!("[cmdline] bootargs found, len={}", copy_len);
            } else {
                if let Some(fallback_cmdline) = fallback_cmdline {
                    let fallback = normalize_boot_info_addr(fallback_cmdline);
                    if let Some((ptr, len)) = parse_raw_cmdline(fallback) {
                        let copy_len = store_cmdline(ptr, len);
                        crate::println!("[cmdline] raw bootargs fallback found, len={}", copy_len);
                    }
                }
            }
        } else if let Some((ptr, len)) = parse_raw_cmdline(boot_info) {
            let copy_len = store_cmdline(ptr, len);
            crate::println!("[cmdline] bootargs found, len={}", copy_len);
        } else {
            if let Some(fallback_cmdline) = fallback_cmdline {
                let fallback = normalize_boot_info_addr(fallback_cmdline);
                if let Some((ptr, len)) = parse_raw_cmdline(fallback) {
                    let copy_len = store_cmdline(ptr, len);
                    crate::println!("[cmdline] raw bootargs fallback found, len={}", copy_len);
                } else {
                    crate::println!("[cmdline] DTB magic mismatch and raw fallback invalid");
                    CMDLINE_INITIALIZED.store(true, Ordering::Release);
                    return;
                }
            } else {
                crate::println!(
                    "[cmdline] DTB magic mismatch at {:#x}: got {:#x}, expected {:#x}",
                    boot_info,
                    magic,
                    FDT_MAGIC
                );
                CMDLINE_INITIALIZED.store(true, Ordering::Release);
                return;
            }
        }
    }

    // Cache debug flags once — eliminates per-signal cmdline scans.
    let cl = get_cmdline();
    SIGFRDBG.store(cl.split_ascii_whitespace().any(|t| t == "SIGFRDBG=1"), Ordering::Relaxed);
    SCDBG_SIG.store(cl.split_ascii_whitespace().any(|t| t == "SCDBG_SIG=1"), Ordering::Relaxed);
    STWDBG.store(cl.contains("STWDBG=1"), Ordering::Relaxed);
    CLDBG.store(cl.split_ascii_whitespace().any(|t| t == "CLDBG=1"), Ordering::Relaxed);
    TICKDBG.store(cl.split_ascii_whitespace().any(|t| t == "TICKDBG=1"), Ordering::Relaxed);
    PFDBG.store(cl.split_ascii_whitespace().any(|t| t == "PFDBG=1"), Ordering::Relaxed);
    WTRACE.store(cl.split_ascii_whitespace().any(|t| t == "WTRACE=1"), Ordering::Relaxed);
    WTRACE_SAVE.store(cl.split_ascii_whitespace().any(|t| t == "WTRACE_SAVE=1"), Ordering::Relaxed);
    SCDBG.store(cl.split_ascii_whitespace().any(|t| t == "SCDBG=1"), Ordering::Relaxed);
    WAITDBG.store(cl.split_ascii_whitespace().any(|t| t == "WAITDBG=1"), Ordering::Relaxed);
    WAITDUMP.store(cl.split_ascii_whitespace().any(|t| t == "WAITDUMP=1"), Ordering::Relaxed);
    HANGDUMP.store(cl.split_ascii_whitespace().any(|t| t == "HANGDUMP=1"), Ordering::Relaxed);
    PIPE_HOFF.store(cl.split_ascii_whitespace().any(|t| t == "PIPE_HANDOFF=1"), Ordering::Relaxed);
    FTXDBG.store(cl.split_ascii_whitespace().any(|t| t == "FTXDBG=1"), Ordering::Relaxed);
    WRDBG.store(cl.split_ascii_whitespace().any(|t| t == "WRDBG=1"), Ordering::Relaxed);
    UDPDBG.store(cl.contains("UDPDBG=1"), Ordering::Relaxed);
    NETDBG.store(cl.contains("NETDBG=1"), Ordering::Relaxed);
    NSDBG.store(cl.contains("NSDBG=1"), Ordering::Relaxed);
    ITDBG.store(cl.split_ascii_whitespace().any(|t| t == "ITDBG=1"), Ordering::Relaxed);
    FANDBG.store(cl.split_ascii_whitespace().any(|t| t == "FANDBG=1"), Ordering::Relaxed);
    H23DBG.store(cl.split_ascii_whitespace().any(|t| t == "H23DBG=1"), Ordering::Relaxed);
    M9DBG.store(cl.split_ascii_whitespace().any(|t| t == "M9DBG=1"), Ordering::Relaxed);
    CL3DBG.store(cl.split_ascii_whitespace().any(|t| t == "CL3DBG=1"), Ordering::Relaxed);
    HUNGDBG.store(cl.contains("HUNGDBG=1"), Ordering::Relaxed);
    FSTRACE.store(cl.contains("FSTRACE=1"), Ordering::Relaxed);
    FTXTRACE.store(cl.contains("FTXTRACE=1"), Ordering::Relaxed);
    LAVMADBG.store(cl.split_ascii_whitespace().any(|t| t == "LAVMADBG=1"), Ordering::Relaxed);
    BSCRASH.store(cl.split_ascii_whitespace().any(|t| t == "BSCRASH=1"), Ordering::Relaxed);
    // FRAMEDBG=1：帧分配/映射取证（map_count 追踪、freed-while-mapped 检测、
    // 内核栈误复用检测）。热路径每页多几次原子操作，仅排查双重分配时开启。
    crate::mm::set_frame_dbg(cl.split_ascii_whitespace().any(|t| t == "FRAMEDBG=1"));
    // SLABDBG=1：对象级 SLUB 双重分配/释放追踪（class-16）。正式 BuildStorm/
    // judge 轮不带该 bootarg，关闭时每次检查只是一次 Relaxed 原子读。
    crate::mm::slab::set_slab_dbg(cl.split_ascii_whitespace().any(|t| t == "SLABDBG=1"));
    // NOSHOOT=1：关闭跨核 TLB shootdown 广播（仅用于性能归因 A/B，见
    // mm::tlb::SHOOTDOWN_ENABLED；正式运行必须保持开启）。
    crate::mm::tlb::set_shootdown_enabled(
        !cl.split_ascii_whitespace().any(|t| t == "NOSHOOT=1"),
    );
    EXECDBG.store(cl.split_ascii_whitespace().any(|t| t == "EXECDBG=1"), Ordering::Relaxed);
    FRAMEPROBE.store(cl.split_ascii_whitespace().any(|t| t == "FRAMEPROBE=1"), Ordering::Relaxed);
    PTESITE.store(cl.split_ascii_whitespace().any(|t| t == "PTESITE=1"), Ordering::Relaxed);
    SPINDBG.store(cl.split_ascii_whitespace().any(|t| t == "SPINDBG=1"), Ordering::Relaxed);
    VMAOPDBG.store(cl.split_ascii_whitespace().any(|t| t == "VMAOPDBG=1"), Ordering::Relaxed);
    SEGVDBG.store(cl.split_ascii_whitespace().any(|t| t == "SEGVDBG=1"), Ordering::Relaxed);
    BLKDBG.store(cl.split_ascii_whitespace().any(|t| t == "BLKDBG=1"), Ordering::Relaxed);

    CMDLINE_INITIALIZED.store(true, Ordering::Release);
}

#[inline]
fn normalize_boot_info_addr(addr: usize) -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        if addr >= 0x8000_0000_0000_0000 {
            addr
        } else {
            0x9000_0000_0000_0000 | addr
        }
    }
    #[cfg(not(target_arch = "loongarch64"))]
    {
        addr
    }
}

unsafe fn store_cmdline(ptr: *const u8, len: usize) -> usize {
    let copy_len = len.min(CMDLINE_MAX - 1);
    core::ptr::copy_nonoverlapping(ptr, CMDLINE_BUF.as_mut_ptr(), copy_len);
    CMDLINE_BUF[copy_len] = 0;
    CMDLINE_LEN.store(copy_len, Ordering::Release);
    copy_len
}

/// 获取内核命令行字符串
///
/// 返回从 DTB bootargs 解析出的命令行。如果未初始化或无 DTB，返回空字符串。
pub fn get_cmdline() -> &'static str {
    let len = CMDLINE_LEN.load(Ordering::Acquire);
    if len == 0 {
        return "";
    }
    unsafe {
        core::str::from_utf8_unchecked(&CMDLINE_BUF[..len])
    }
}

/// 从 FDT 中解析 bootargs 属性
///
/// 返回 (属性值指针, 属性值长度)，不包含末尾的 null 字节。
unsafe fn parse_fdt_bootargs(dtb: usize) -> Option<(*const u8, usize)> {
    let header = &*(dtb as *const FdtHeader);

    // 验证 magic
    let magic = u32::from_be(header.magic);
    if magic != FDT_MAGIC {
        return None;
    }

    let struct_offset = u32::from_be(header.off_dt_struct) as usize;
    let strings_offset = u32::from_be(header.off_dt_strings) as usize;

    let struct_base = dtb + struct_offset;
    let strings_base = dtb + strings_offset;

    // 遍历 structure block 寻找 /chosen 节点中的 bootargs
    let mut pos = struct_base;
    let mut in_chosen = false;
    let mut depth: i32 = 0;
    let mut chosen_depth: i32 = -1;

    loop {
        let token = u32::from_be(read_u32(pos));
        pos += 4;

        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                // 读取节点名（null-terminated string）
                let name_start = pos;
                let name_len = strlen(pos as *const u8);
                let name = core::slice::from_raw_parts(name_start as *const u8, name_len);

                // 调试：打印前几个节点名
                if depth <= 2 && name_len < 64 {
                    if let Ok(s) = core::str::from_utf8(name) {
                        crate::println!("[cmdline] FDT node depth={}: '{}'", depth, s);
                    }
                }

                // 检查是否是 "chosen" 节点
                if name_len >= 6 && &name[..6] == b"chosen" {
                    in_chosen = true;
                    chosen_depth = depth;
                    crate::println!("[cmdline] Found /chosen at depth={}", depth);
                }

                // 跳过节点名（4 字节对齐）
                pos += align4(name_len + 1); // +1 for null terminator
            }
            FDT_END_NODE => {
                if in_chosen && depth == chosen_depth {
                    in_chosen = false;
                    chosen_depth = -1;
                }
                depth -= 1;
            }
            FDT_PROP => {
                // FDT_PROP 格式: u32 len, u32 nameoff, [data aligned to 4]
                let prop_len = u32::from_be(read_u32(pos)) as usize;
                pos += 4;
                let nameoff = u32::from_be(read_u32(pos)) as usize;
                pos += 4;

                if in_chosen {
                    // 检查属性名是否为 "bootargs"
                    let prop_name_ptr = (strings_base + nameoff) as *const u8;
                    if streq(prop_name_ptr, b"bootargs\0") {
                        // 找到了 bootargs 属性
                        let value_ptr = pos as *const u8;
                        // prop_len 包含 null terminator，我们返回不含 null 的长度
                        let value_len = if prop_len > 0 && *value_ptr.add(prop_len - 1) == 0 {
                            prop_len - 1
                        } else {
                            prop_len
                        };
                        return Some((value_ptr, value_len));
                    }
                }

                // 跳过属性值（4 字节对齐）
                pos += align4(prop_len);
            }
            FDT_NOP => {
                // 跳过
            }
            FDT_END => {
                break;
            }
            _ => {
                // 未知 token，停止解析
                break;
            }
        }
    }

    None
}

unsafe fn parse_fdt_cpu_count(dtb: usize) -> usize {
    let header = &*(dtb as *const FdtHeader);

    let magic = u32::from_be(header.magic);
    if magic != FDT_MAGIC {
        return 0;
    }

    let struct_offset = u32::from_be(header.off_dt_struct) as usize;
    let strings_offset = u32::from_be(header.off_dt_strings) as usize;

    let struct_base = dtb + struct_offset;
    let strings_base = dtb + strings_offset;

    let mut pos = struct_base;
    let mut depth: i32 = 0;
    let mut in_cpus = false;
    let mut cpus_depth: i32 = -1;
    let mut cpu_node_depth: i32 = -1;
    let mut cpu_node_counted = false;
    let mut count = 0usize;

    loop {
        let token = u32::from_be(read_u32(pos));
        pos += 4;

        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                let name_start = pos;
                let name_len = strlen(pos as *const u8);
                let name = core::slice::from_raw_parts(name_start as *const u8, name_len);

                if name == b"cpus" {
                    in_cpus = true;
                    cpus_depth = depth;
                } else if in_cpus && depth == cpus_depth + 1 {
                    cpu_node_depth = depth;
                    cpu_node_counted = false;
                    if name == b"cpu" || name.starts_with(b"cpu@") {
                        count += 1;
                        cpu_node_counted = true;
                    }
                }

                pos += align4(name_len + 1);
            }
            FDT_END_NODE => {
                if cpu_node_depth == depth {
                    cpu_node_depth = -1;
                    cpu_node_counted = false;
                }
                if in_cpus && depth == cpus_depth {
                    in_cpus = false;
                    cpus_depth = -1;
                }
                depth -= 1;
            }
            FDT_PROP => {
                let prop_len = u32::from_be(read_u32(pos)) as usize;
                pos += 4;
                let nameoff = u32::from_be(read_u32(pos)) as usize;
                pos += 4;

                if in_cpus && depth == cpu_node_depth && !cpu_node_counted {
                    let prop_name_ptr = (strings_base + nameoff) as *const u8;
                    if streq(prop_name_ptr, b"device_type\0") && prop_len >= 3 {
                        let value = core::slice::from_raw_parts(pos as *const u8, prop_len);
                        if value.starts_with(b"cpu") {
                            count += 1;
                            cpu_node_counted = true;
                        }
                    }
                }

                pos += align4(prop_len);
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => break,
        }
    }

    count
}

unsafe fn parse_raw_cmdline(addr: usize) -> Option<(*const u8, usize)> {
    let ptr = addr as *const u8;
    let mut len = 0;
    while len < CMDLINE_MAX - 1 {
        let byte = *ptr.add(len);
        if byte == 0 {
            break;
        }
        if byte != b'\t' && (byte < 0x20 || byte > 0x7e) {
            return None;
        }
        len += 1;
    }
    if len == 0 {
        None
    } else {
        Some((ptr, len))
    }
}

/// 读取一个大端 u32
#[inline]
unsafe fn read_u32(addr: usize) -> u32 {
    core::ptr::read_unaligned(addr as *const u32)
}

/// 计算 null-terminated 字符串长度
#[inline]
unsafe fn strlen(ptr: *const u8) -> usize {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    len
}

/// 比较内存中的字符串与给定字节序列（包含 null terminator）
#[inline]
unsafe fn streq(ptr: *const u8, expected: &[u8]) -> bool {
    for (i, &byte) in expected.iter().enumerate() {
        if *ptr.add(i) != byte {
            return false;
        }
    }
    true
}

/// 向上对齐到 4 字节
#[inline]
const fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// 探测 RISC-V Zicboz 扩展（`cbo.zero` 整 cache 块清零）。
///
/// 解析 FDT cpu 节点的 `riscv,isa` / `riscv,isa-extensions` 是否含
/// `zicboz`，且 `riscv,cboz-block-size`（缺省 64）为 64 时才启用。
/// QEMU virt rv64 默认开启 Zicboz 且 OpenSBI 置 menvcfg.CBZE，
/// S 态可直接执行；真机/其它固件下 isa 串缺失即自动回退 memset。
#[cfg(target_arch = "riscv64")]
pub fn detect_zicboz(dtb: usize) -> bool {
    let boot_info = normalize_boot_info_addr(dtb);
    unsafe { parse_fdt_zicboz(boot_info) }
}

#[cfg(target_arch = "riscv64")]
unsafe fn parse_fdt_zicboz(dtb: usize) -> bool {
    let header = &*(dtb as *const FdtHeader);
    if u32::from_be(header.magic) != FDT_MAGIC {
        return false;
    }
    let struct_base = dtb + u32::from_be(header.off_dt_struct) as usize;
    let strings_base = dtb + u32::from_be(header.off_dt_strings) as usize;
    let mut pos = struct_base;
    let mut has_zicboz = false;
    let mut block_size = 64usize;
    loop {
        let token = u32::from_be(read_u32(pos));
        pos += 4;
        match token {
            FDT_BEGIN_NODE => {
                let name_len = strlen(pos as *const u8);
                pos += align4(name_len + 1);
            }
            FDT_END_NODE => {}
            FDT_PROP => {
                let prop_len = u32::from_be(read_u32(pos)) as usize;
                pos += 4;
                let nameoff = u32::from_be(read_u32(pos)) as usize;
                pos += 4;
                let prop_name_ptr = (strings_base + nameoff) as *const u8;
                if streq(prop_name_ptr, b"riscv,isa\0")
                    || streq(prop_name_ptr, b"riscv,isa-extensions\0")
                {
                    let value = core::slice::from_raw_parts(pos as *const u8, prop_len);
                    if value.windows(6).any(|w| w == b"zicboz") {
                        has_zicboz = true;
                    }
                } else if streq(prop_name_ptr, b"riscv,cboz-block-size\0") && prop_len >= 4 {
                    block_size = u32::from_be(read_u32(pos)) as usize;
                }
                pos += align4(prop_len);
            }
            FDT_NOP => {}
            _ => break,
        }
    }
    has_zicboz && block_size == 64
}

/// 从 FDT `/memory` 节点解析出主 RAM bank 的末尾物理地址（base + size）。
///
/// 返回第一个名为 `memory` 或 `memory@...` 的节点 `reg` 描述的 base+size；
/// 找不到或 DTB 无效时返回 `None`。供 `mm::init` 在分配器初始化前探测真实
/// 物理内存大小，从而用满 QEMU `-m` 给定的内存（而非写死的编译期上限）。
pub fn detect_ram_end(dtb: usize) -> Option<usize> {
    let boot_info = normalize_boot_info_addr(dtb);
    unsafe { parse_fdt_memory_end(boot_info) }
}

/// 遍历 FDT，解析全部 `/memory` 节点的 `reg` 属性，返回所有 RAM bank 中
/// 最大的 base + size。
///
/// RISC-V virt 与 LoongArch virt 的根节点均为 `#address-cells = <2>`、
/// `#size-cells = <2>`，即 `reg = <base_hi base_lo size_hi size_lo>`（大端 u32）。
/// 同时兼容 1/1 cells（`reg` 仅 8 字节）的退化情况。
///
/// 必须遍历**所有**节点和**每个** reg 里的**全部** (base,size) 对，而不是
/// 返回第一个：QEMU LoongArch virt 把 RAM 拆成低 bank（256MB @0）+ 高 bank
/// （@0x8000_0000），低 bank 节点在前——只取第一个会把 8G 机器认成 256MB。
unsafe fn parse_fdt_memory_end(dtb: usize) -> Option<usize> {
    let header = &*(dtb as *const FdtHeader);
    if u32::from_be(header.magic) != FDT_MAGIC {
        return None;
    }

    let struct_base = dtb + u32::from_be(header.off_dt_struct) as usize;
    let strings_base = dtb + u32::from_be(header.off_dt_strings) as usize;

    let mut pos = struct_base;
    let mut depth: i32 = 0;
    let mut in_memory = false;
    let mut memory_depth: i32 = -1;
    let mut max_end: Option<usize> = None;

    loop {
        let token = u32::from_be(read_u32(pos));
        pos += 4;

        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                let name_len = strlen(pos as *const u8);
                let name = core::slice::from_raw_parts(pos as *const u8, name_len);
                if name == b"memory" || name.starts_with(b"memory@") {
                    in_memory = true;
                    memory_depth = depth;
                }
                pos += align4(name_len + 1);
            }
            FDT_END_NODE => {
                if in_memory && depth == memory_depth {
                    in_memory = false;
                    memory_depth = -1;
                }
                depth -= 1;
            }
            FDT_PROP => {
                let prop_len = u32::from_be(read_u32(pos)) as usize;
                pos += 4;
                let nameoff = u32::from_be(read_u32(pos)) as usize;
                pos += 4;

                if in_memory {
                    let prop_name_ptr = (strings_base + nameoff) as *const u8;
                    if streq(prop_name_ptr, b"reg\0") {
                        if prop_len >= 16 {
                            // #address-cells = 2, #size-cells = 2；reg 可含多对
                            let mut off = 0;
                            while off + 16 <= prop_len {
                                let base_hi = u32::from_be(read_u32(pos + off)) as usize;
                                let base_lo = u32::from_be(read_u32(pos + off + 4)) as usize;
                                let size_hi = u32::from_be(read_u32(pos + off + 8)) as usize;
                                let size_lo = u32::from_be(read_u32(pos + off + 12)) as usize;
                                let base = (base_hi << 32) | base_lo;
                                let size = (size_hi << 32) | size_lo;
                                if size > 0 {
                                    let end = base + size;
                                    if max_end.is_none_or(|m| end > m) {
                                        max_end = Some(end);
                                    }
                                }
                                off += 16;
                            }
                        } else if prop_len >= 8 {
                            // #address-cells = 1, #size-cells = 1
                            let base = u32::from_be(read_u32(pos)) as usize;
                            let size = u32::from_be(read_u32(pos + 4)) as usize;
                            if size > 0 {
                                let end = base + size;
                                if max_end.is_none_or(|m| end > m) {
                                    max_end = Some(end);
                                }
                            }
                        }
                    }
                }

                pos += align4(prop_len);
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => break,
        }
    }

    max_end
}
