//! RustMicroOS 内核入口
//!
//! 这是一个面向 RISC-V 架构的 Rust 微内核操作系统。
//!
//! # 特性
//! - 能力安全系统 (Capability-based Security)
//! - 异步内核运行时
//! - 微内核架构
//! - 用户态设备驱动

#![no_std]
#![no_main]
#![feature(panic_info_message)]
#![feature(alloc_error_handler)]
#![feature(naked_functions)]
#![feature(inline_const)]
#![feature(generic_const_exprs)]
#![allow(incomplete_features)]
// 注意：全局 #![allow(dead_code)] 已移除
// 各模块内部使用局部 #[allow(dead_code)] 标记保留用于教学和未来扩展的 API

extern crate alloc;

/// rtdiag 实时诊断探针总开关：true 时打印各 [rtdiag] 延迟探针，
/// 定位 cyclictest 类实时延迟根因；正式构建保持 false（编译期消除）。
pub const RTDIAG: bool = false;

/// 诊断实验开关：跳过 unmap_region 尾部 flush_tlb_all（测量 QEMU LA
/// invtlb→softmmu 全清→refill 风暴的量级；正确性不安全，仅测量用，
/// 正式构建必须为 false）。
pub const DIAG_SKIP_UNMAP_FLUSH: bool = false;

/// 诊断实验开关：跳过 syscall 出口的 CPU 时间分账（account_syscall_combined_exit
/// 的锁 + rdtime + itimer/rlimit 检查），量化其在 syscall 热路径中的指令占比；
/// 会破坏 utime/stime/itimer/RLIMIT_CPU 语义，仅测量用，正式构建必须为 false。
pub const DIAG_SKIP_ACCT: bool = false;

/// rtdiag 专用打印：RTDIAG=false 时整体折叠为空（零开销）。
#[macro_export]
macro_rules! rtdiag_println {
    ($($t:tt)*) => {
        if $crate::RTDIAG {
            $crate::println!($($t)*);
        }
    };
}

// 核心模块 - 这些模块的功能已真正集成
mod arch;
mod percpu;  // 统一 per-CPU 底座（MAX_CPUS / current_cpu_id）
mod console;
pub mod error;  // 统一错误处理
mod lang_items;
mod mem_ops;   // word-at-a-time memcpy/memset/memcmp（覆盖 compiler_builtins 逐字节版）
mod mm;
mod capability;
mod ipc;
mod sched;
mod sync;
mod async_rt;
mod seccomp;  // 真实 cBPF seccomp(2) 过滤器（POSIX/Linux 兼容）
mod syscall;
pub mod kernel;
pub mod security;
mod exec;
mod lockfree;
mod driver;
mod fs;
mod random;
pub mod loader;  // ELF 加载器

// 高级功能模块 - 部分功能已集成，其余保留用于教学和未来扩展
// 各模块内部使用局部 #[allow(dead_code)] 处理未使用的 API
mod hypervisor;
mod ebpf;
mod livepatch;
mod net;
mod power;
mod secure;  // 已集成：启动验证、度量功能
mod io_uring;
mod cheri;
mod gdb;     // 已集成：trap 异常处理
mod formal;  // 已集成：能力操作验证、报告生成
mod safety;
pub mod config;
pub mod cmdline;    // 内核命令行（DTB bootargs）
pub mod benchmark;  // 性能基准测试框架

use core::arch::global_asm;

// 引入汇编入口代码
#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("arch/riscv64/entry.S"));

/// 内核主入口（从汇编跳转到这里）
#[no_mangle]
pub extern "C" fn kernel_main(hartid: usize, dtb: usize) -> ! {
    kernel_main_with_cmdline(hartid, dtb, None)
}

/// Enter the shared kernel path with an optional raw boot command line.
///
/// `raw_cmdline` is the physical address passed in LoongArch `a1` by QEMU's
/// direct-kernel boot ABI. RISC-V callers pass `None`; LoongArch passes
/// `Some(a1)` so `SINGLE_CMD` and `SINGLE_CWD` remain available when the DTB
/// has no `/chosen/bootargs` property.
pub fn kernel_main_with_cmdline(
    hartid: usize,
    dtb: usize,
    raw_cmdline: Option<usize>,
) -> ! {
    // 清除 BSS 段
    clear_bss();

    // 初始化控制台（UART）
    console::init();

    println!();
    println!("========================================");
    println!("  RustMicroOS v0.1.0");
    #[cfg(target_arch = "riscv64")]
    println!("  A Rust Microkernel for RISC-V");
    #[cfg(target_arch = "loongarch64")]
    println!("  A Rust Microkernel for LoongArch64");
    println!("========================================");
    println!();
    println!("[kernel] Boot on hart {}", hartid);
    println!("[kernel] DTB at {:#x}", dtb);

    // 初始化内存管理
    mm::init(dtb);
    println!("[kernel] Memory manager initialized");

    // 解析 DTB 中的内核命令行（需要在 mm::init 之后，此时可安全访问 DTB 内存）
    cmdline::init_with_fallback(dtb, raw_cmdline);
    #[cfg(target_arch = "riscv64")]
    if cmdline::detect_zicboz(dtb) {
        mm::enable_zicboz();
        println!("[kernel] Zicboz detected: cbo.zero page zeroing enabled");
    }
    let cmdline_str = cmdline::get_cmdline();
    if !cmdline_str.is_empty() {
        println!("[kernel] Kernel cmdline: {}", cmdline_str);
    }

    // 初始化能力系统
    capability::init();
    println!("[kernel] Capability system initialized");

    mm::percpu_cache::set_cpu_id(hartid);

    // 初始化调度器
    sched::init(hartid);
    println!("[kernel] Scheduler initialized");

    // 初始化 IPC 子系统
    ipc::init();
    println!("[kernel] IPC subsystem initialized");

    // 初始化异步运行时
    async_rt::init();
    println!("[kernel] Async runtime initialized");

    // 初始化系统调用
    println!("[kernel] Initializing syscall layer...");
    syscall::init();
    println!("[kernel] Syscall layer initialized");

    // 初始化驱动框架
    println!("[kernel] Initializing drivers...");
    driver::init();
    net::socket::init();
    net::net_stack::init();

    // 初始化文件系统
    fs::init();
    random::init();

    // 根据块设备数量决定挂载策略
    // 竞赛环境: bus.0 = sdcard (ext4), bus.1 = disk.img (FAT32)
    // 本地测试: 通常只有 bus.0 = disk.img (FAT32)
    let blk_count = driver::virtio::blk::device_count();
    println!("[kernel] Found {} block device(s)", blk_count);

    if blk_count >= 2 {
        // 竞赛环境：先尝试 ext4 挂载 sdcard (设备0)
        println!("[kernel] Attempting ext4 mount on device 0 (sdcard)...");
        if let Err(e) = fs::ext4::mount_ext4(0, "/") {
            println!("[kernel] ext4 mount failed: {}, trying FAT32...", e);
            if let Err(e2) = fs::mount_fat32() {
                println!("[kernel] FAT32 mount also failed: {}", e2);
            }
        }
    } else if blk_count == 1 {
        // 本地测试：先试 ext4，失败则试 FAT32
        println!("[kernel] Attempting ext4 mount on device 0...");
        if let Err(_) = fs::ext4::mount_ext4(0, "/") {
            println!("[kernel] ext4 failed, trying FAT32...");
            if let Err(e) = fs::mount_fat32() {
                println!("[kernel] FAT32 mount skipped: {}", e);
            } else {
                println!("[kernel] FAT32 mounted successfully");
            }
        }
    } else {
        println!("[kernel] No block devices found, filesystem unavailable");
    }

    let _ = fs::create_dir_all("/tmp");
    let _ = fs::create_dir_all("/var/tmp");
    // /tmp 挂真 tmpfs（/proc/mounts 早已如实声称 tmpfs）：ext4 根上的 /tmp
    // 目录只作挂载点。此后 /tmp 下 create/unlink/lookup 全走纯内存 RamFs，
    // 不再触发 ext4 底层目录查询与 virtio 读盘（lmbench lat_fs 的读盘 +
    // DMA 轮询开销全消）。镜像内 /tmp 本身为空目录，遮蔽无损。
    fs::mount("/tmp", fs::RamFs::new());
    // Linux /tmp and /var/tmp are world-writable with sticky bit (mode 1777)
    if let Ok(tmp_inode) = fs::lookup("/tmp") {
        let _ = tmp_inode.set_mode(0o41777); // S_IFDIR | 01777
    }
    if let Ok(tmp_inode) = fs::lookup("/var/tmp") {
        let _ = tmp_inode.set_mode(0o41777);
    }
    let _ = fs::create_dir_all("/proc");
    fs::devfs::register_builtin_devices();
    fs::devfs::mount_devfs();
    fs::mount("/dev/shm", fs::RamFs::new());
    fs::mount("/dev/pts", fs::RamFs::new());
    fs::procfs::mount_procfs();
    // Mount sysfs at /sys (per Linux: mounted at boot by init)
    let _ = fs::create_dir_all("/sys");
    fs::mount("/sys", fs::sysfs::SysFs::new());
    fs::mount_info_add("sysfs", "/sys", "sysfs", 0);

    // cgroup v1 memory controller hierarchy, pre-mounted as a distro's init
    // would (tmpfs at /sys/fs/cgroup + the memory controller under it). LTP
    // `madvise09` only `access()`-checks `/sys/fs/cgroup/memory/`, never mounts
    // it — so it must already exist. Backed by mm::memcg (real limits + reclaim).
    fs::mount("/sys/fs/cgroup", fs::RamFs::new());
    fs::mount_info_add("tmpfs", "/sys/fs/cgroup", "tmpfs", 0);
    let _ = fs::create_dir_all("/sys/fs/cgroup/memory");
    fs::mount("/sys/fs/cgroup/memory", fs::memcgfs::MemcgFs::new());
    fs::mount_info_add("cgroup", "/sys/fs/cgroup/memory", "cgroup", 0);

    // seed Linux-standard /etc files into ext4 overlay so LTP getpwnam("nobody")
    // 与 gethostname、gethostbyname、getservbyname、getprotoent 等不会 ENOENT。
    // 仅适用于 ext4 挂载（FAT32 不使用 overlay）。
    fs::ext4::seed_overlay_etc("/");

    // 初始化 Hypervisor
    if config::is_hypervisor_enabled() {
        hypervisor::init();
    }

    // 初始化 eBPF 子系统
    if config::is_ebpf_enabled() {
        ebpf::init();
    }

    // 初始化热补丁系统
    if config::is_livepatch_enabled() {
        livepatch::init();
    }

    // 初始化网络子系统
    net::net_stack::reload_dns_config();

    // 初始化电源管理（真正集成）
    if config::is_power_enabled() {
        power::init();
        // 设置默认电源策略为平衡模式
        power::set_policy(power::PowerPolicy::Balanced);
    }

    // 初始化安全子系统（真正集成）
    if config::is_secure_enabled() {
        secure::init();
        // 验证启动链完整性
        if let Err(e) = secure::verify_boot_chain() {
            println!("[kernel] WARNING: Boot chain verification failed: {:?}", e);
            // 在生产环境中可能需要停止启动
            // panic!("Boot chain verification failed");
        }

        // 记录初始化阶段的启动度量
        secure::measure_boot_stage(
            secure::BootStage::Init,
            b"RustMicroOS Init Stage",
            "Kernel initialization complete"
        );
    }

    // 初始化 Rust 安全增强
    safety::init();

    // 初始化 io_uring 子系统
    if config::is_io_uring_enabled() {
        io_uring::init();
    }

    // 初始化 CHERI 能力模拟
    if config::is_cheri_enabled() {
        cheri::init();
    }

    // 初始化 GDB Stub（真正集成）
    if config::is_gdb_enabled() {
        gdb::init();
        // GDB Stub 已准备好接受连接
    }

    // 初始化 ProcFS
    fs::procfs::init();

    // 初始化形式化验证规约（真正集成）
    if config::is_formal_enabled() {
        formal::init();
        // 形式化规约已加载，可用于运行时验证

        // 生成并打印验证报告摘要
        let report = formal::verification::VerificationReportGenerator::generate_full_report();
        let verified_count = report.properties.iter()
            .filter(|p| p.status == formal::verification::PropertyStatus::Verified)
            .count();
        println!("[kernel] Formal verification: {} properties verified", verified_count);

        // 在调试模式下打印详细报告
        #[cfg(debug_assertions)]
        if config::get_debug_level() >= 4 {
            formal::print_verification_report(&report);
        }
    }

    // 挂载 ramfs 作为根文件系统（仅当 FAT32 未挂载时）
    // 注意：FAT32 已经在前面挂载到 "/" 了，这里只是作为备用
    // 如果 FAT32 挂载成功，这里不需要再挂载 ramfs
    // let ramfs = fs::ramfs::RamFs::new();
    // fs::mount("/", ramfs);
    println!("[kernel] Root filesystem ready");

    // 初始化中断处理
    #[cfg(target_arch = "riscv64")]
    arch::riscv64::trap::init();
    #[cfg(target_arch = "loongarch64")]
    arch::loongarch64::trap::init();

    // 初始化 TLB 管理器
    mm::tlb::init(config::get_num_cpus());

    println!();
    println!("[kernel] All subsystems initialized successfully!");

    // 启动从核（SMP 多核支持）
    let num_cpus = config::get_num_cpus();
    if num_cpus > 1 {
        println!("[kernel] Starting {} secondary harts, boot_hart={}...", num_cpus - 1, hartid);
        for cpu_id in 0..num_cpus {
            if cpu_id == hartid {
                continue;
            }
            match sched::smp::boot_ap(cpu_id) {
                Ok(()) => println!("[kernel] Hart {} boot initiated", cpu_id),
                Err(e) => println!("[kernel] Failed to boot hart {}: {}", cpu_id, e),
            }
        }
    }

    // 运行子系统自测试
    // 在 debug 模式下默认运行，在 release 模式下可通过配置启用
    // DISABLED: Self-tests take too long, skipping for faster boot
    /*
    let run_self_tests = {
        #[cfg(debug_assertions)]
        { true }
        #[cfg(not(debug_assertions))]
        { config::is_self_test_enabled() }
    };

    if run_self_tests {
        println!();
        println!("[kernel] Running subsystem self-tests...");
        arch::self_test();
        mm::self_test();
        mm::pmem::self_test();
        capability::self_test();
        sched::self_test();
        ipc::self_test();
        fs::self_test();
        io_uring::self_test().expect("io_uring self-test failed");
        cheri::self_test().expect("cheri self-test failed");
        safety::self_test();
        gdb::self_test();
        formal::self_test();
        driver::self_test().expect("driver self-test failed");
        ebpf::self_test().expect("ebpf self-test failed");
        net::self_test().expect("net self-test failed");
        power::self_test().expect("power self-test failed");
        secure::self_test().expect("secure self-test failed");
        livepatch::self_test().expect("livepatch self-test failed");
        hypervisor::self_test().expect("hypervisor self-test failed");
        error::self_test();
        loader::self_test().expect("loader self-test failed");
        loader::checkpoint_5_verify_elf_loading().expect("loader checkpoint 5 failed");
        println!("[kernel] All self-tests passed!");
    }
    */
    println!("[kernel] Self-tests disabled for faster boot");

    println!("[kernel] Entering main loop...");

    // 启动 init 进程
    // Requirements 6.4: 如果 /init 不存在，打印错误并 halt
    if let Err(e) = start_init_process() {
        println!("[kernel] FATAL: Failed to start init process: {}", e);
        println!("[kernel] System cannot continue without init process.");
        println!("[kernel] Entering halt loop...");
        loop {
            arch::wfi();
        }
    }

    // `start_init_process()` 成功时经 `usertrapret_by_pid` 直接进入用户态，不返回；
    // 走到这里说明 init 已让出 CPU，BSP 与所有 AP 一样进入统一的 idle+调度循环。
    //
    // 这里原本是一个遗留的伪调度主循环（`sched::schedule()` + 已删除的
    // `get_task_pid()` 恒 None + `power::enter_idle`），既不可达也不执行任何任务；
    // 真正的任务选择/切换/用户态返回全部在 `kernel_idle_until_runnable` 中完成。
    sched::enter_idle_scheduler_loop()
}

/// 启动 init 进程
///
/// 竞赛模式: sdcard 上没有 /init，使用内嵌 ELF。
/// 本地模式: disk.img 中有 /init，优先使用。
fn start_init_process() -> Result<(), &'static str> {
    use sched::process;
    #[cfg(target_arch = "riscv64")]
    use arch::riscv64::trap::usertrapret_by_pid;
    #[cfg(target_arch = "loongarch64")]
    use arch::loongarch64::trap::usertrapret_by_pid;

    println!();
    println!("[kernel] Starting init process...");

    // 使用内嵌 ELF（竞赛用 init，负责执行 /musl/basic/ 测试）
    let init_pid = {
        #[cfg(target_arch = "riscv64")]
        static INIT_ELF: &[u8] = include_bytes!(
            concat!(env!("CARGO_MANIFEST_DIR"),
                    "/../target/riscv64gc-unknown-none-elf/release/init")
        );
        #[cfg(target_arch = "loongarch64")]
        static INIT_ELF: &[u8] = include_bytes!(
            concat!(env!("CARGO_MANIFEST_DIR"),
                    "/../target/loongarch64-unknown-none/release/init")
        );
        let pid = process::create_process_from_elf("init", INIT_ELF)?;
        println!("[kernel] Created embedded init, PID {}", pid);
        pid
    };

    println!("[kernel] Entering user mode...");
    usertrapret_by_pid(init_pid);
}
// 这个函数整合了 ELF 加载、地址空间创建、TrapContext 初始化等所有步骤

/// 清除 BSS 段
fn clear_bss() {
    extern "C" {
        fn sbss();
        fn ebss();
    }
    // 链接脚本保证 sbss/ebss 8 字节对齐；按 u64 清零（逐字节 fill 在 ~99MB BSS 上
    // 要烧 4 亿条指令、boot 慢数百毫秒）。
    unsafe {
        let start = sbss as usize;
        let end = ebss as usize;
        let mut p = start as *mut u64;
        let words = (end - start) / 8;
        for _ in 0..words {
            p.write_volatile(0);
            p = p.add(1);
        }
        let mut b = (start + words * 8) as *mut u8;
        while (b as usize) < end {
            b.write_volatile(0);
            b = b.add(1);
        }
    }
}

/// 从核（Secondary Hart）入口函数
///
/// 当从核被唤醒时，从汇编代码跳转到这里。
/// 负责初始化从核的本地状态并进入调度循环。
///
/// # 参数
/// - `hartid`: 当前 hart 的 ID
#[no_mangle]
pub extern "C" fn secondary_hart_main(hartid: usize) -> ! {
    // 设置当前 CPU ID
    sched::smp::set_current_cpu(hartid);
    mm::percpu_cache::set_cpu_id(hartid);

    println!("[kernel] Secondary hart {} started", hartid);

    // 初始化本地中断
    #[cfg(target_arch = "riscv64")]
    arch::riscv64::trap::init_secondary();
    #[cfg(target_arch = "loongarch64")]
    arch::loongarch64::trap::init_secondary();

    // 通知 SMP 管理器此 hart 已上线
    sched::smp::ap_online(hartid);

    println!("[kernel] Hart {} entering scheduler loop", hartid);

    // 真正参与执行：AP 进入与 BSP 相同的 idle+调度循环，从全局就绪队列取任务
    // 并在本 hart 上运行（对标 Linux 每个 CPU 的 `cpu_startup_entry`）。
    //
    // 此前这里是一个「取 tid 但从不执行」的死循环（`get_task_pid` 恒为 None，
    // `// TODO: 执行任务`），因此无论 `-smp` 给多少核，全系统只有 BSP 在跑用户
    // 任务：BuildStorm 的数百 crate 真实 Rust 编译被压成单核串行，宿主侧观测
    // 到 QEMU 稳定只占约 100% CPU（`docker stats`: `101.87%`，8 vCPU 满载应
    // 接近 800%），而官方时间基线 B 是 Linux 在同样 `-smp 8` 下的耗时。
    //
    // 本 hart 的定时器保持开启：idle 的 wfi 由 timer tick 唤醒后重新查就绪队列
    // （对标未开 NOHZ 的 Linux idle），执行任务期间 tick 提供抢占点。
    sched::enter_idle_scheduler_loop()
}
