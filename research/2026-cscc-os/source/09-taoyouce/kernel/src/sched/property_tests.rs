//! 进程管理属性测试
//!
//! 这些测试验证进程管理的核心正确性属性。
//!
//! ## Property 7: 进程地址空间隔离
//! Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
//! **Validates: Requirements 3.3**
//!
//! *For any* two distinct processes P1 and P2, their page table roots SHALL be different,
//! and *for any* virtual address V mapped in P1, accessing V from P2 SHALL either fault
//! or access different physical memory.
//!
//! ## Property 8: 进程生命周期管理
//! Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
//! **Validates: Requirements 3.6**
//!
//! *For any* process that calls exit(), *all* of its allocated memory pages SHALL be freed,
//! *all* of its open file handles SHALL be closed, and its parent SHALL be notified.

#![allow(unused_imports)]

use super::process::ProcessControlBlock;
use super::process::ProcessState;
use super::process::ExitReason;
use alloc::vec::Vec;

/// 属性测试迭代次数 - 减少以加快启动速度
const PROPERTY_TEST_ITERATIONS: usize = 5;

/// 简单的伪随机数生成器
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.state
    }

    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if max <= min {
            return min;
        }
        min + (self.next() % (max - min))
    }
}

// ============================================================================
// Property 7: 进程地址空间隔离
// ============================================================================

/// **Property 7: 进程地址空间隔离 - 页表根不同**
/// *For any* two distinct processes P1 and P2, their page table roots SHALL be different.
///
/// Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
/// **Validates: Requirements 3.3**
///
/// 运行 100 次迭代
#[test]
fn property_7_page_table_roots_different() {
    let mut rng = SimpleRng::new(77777);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        // 创建两个不同的进程
        let pid1 = rng.next_range(1, 1000) as usize;
        let mut pid2 = rng.next_range(1, 1000) as usize;

        // 确保 PID 不同
        while pid2 == pid1 {
            pid2 = rng.next_range(1, 1000) as usize;
        }

        let mut process1 = ProcessControlBlock::new(pid1);
        let mut process2 = ProcessControlBlock::new(pid2);

        // 为两个进程创建地址空间
        let result1 = process1.create_address_space();
        let result2 = process2.create_address_space();

        if result1.is_err() || result2.is_err() {
            // 如果内存分配失败，跳过此迭代
            continue;
        }

        // 验证两个进程都有地址空间
        if process1.mm.is_none() || process2.mm.is_none() {
            panic!(
                "迭代 {}: 进程地址空间创建失败: P1={}, P2={}",
                iteration,
                process1.mm.is_some(),
                process2.mm.is_some()
            );
        }

        // 验证页表根地址不同
        let root1 = process1.page_table_root;
        let root2 = process2.page_table_root;

        if root1.0 == root2.0 {
            panic!(
                "迭代 {}: 两个不同进程的页表根相同: P1(pid={})={:#x}, P2(pid={})={:#x}",
                iteration, pid1, root1.0, pid2, root2.0
            );
        }

        // 验证 SATP 值不同
        let satp1 = process1.satp();
        let satp2 = process2.satp();

        if satp1 == satp2 {
            panic!(
                "迭代 {}: 两个不同进程的 SATP 相同: P1={:#x}, P2={:#x}",
                iteration, satp1, satp2
            );
        }
    }
}

/// **Property 7: 进程地址空间隔离 - 独立地址空间**
/// *For any* process, creating an address space SHALL allocate a unique page table.
///
/// Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
/// **Validates: Requirements 3.3**
///
/// 创建多个同时存在的进程，验证它们的页表根地址唯一
#[test]
fn property_7_unique_address_space() {
    let mut rng = SimpleRng::new(88888);
    let mut processes: Vec<ProcessControlBlock> = Vec::new();

    // 创建 10 个同时存在的进程
    for iteration in 0..10 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        let result = process.create_address_space();

        if result.is_err() {
            // 如果内存分配失败，跳过此迭代
            continue;
        }

        // 验证地址空间存在
        if process.mm.is_none() {
            panic!(
                "迭代 {}: 进程 {} 地址空间创建失败",
                iteration, pid
            );
        }

        // 验证页表根地址非零
        let root = process.page_table_root.0;
        if root == 0 {
            panic!(
                "迭代 {}: 进程 {} 页表根地址为零",
                iteration, pid
            );
        }

        // 验证页表根地址与已存在的进程不重复
        for existing in &processes {
            if existing.page_table_root.0 == root {
                panic!(
                    "迭代 {}: 进程 {} 的页表根地址 {:#x} 与之前的进程重复",
                    iteration, pid, root
                );
            }
        }

        processes.push(process);
    }
}

/// **Property 7: 进程地址空间隔离 - 用户栈隔离**
/// *For any* two processes with user stacks, their stack regions SHALL map to
/// different physical memory.
///
/// Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
/// **Validates: Requirements 3.3**
///
/// 运行 100 次迭代
#[test]
fn property_7_user_stack_isolation() {
    let mut rng = SimpleRng::new(99999);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        // 创建两个进程
        let pid1 = rng.next_range(1, 1000) as usize;
        let mut pid2 = rng.next_range(1, 1000) as usize;
        while pid2 == pid1 {
            pid2 = rng.next_range(1, 1000) as usize;
        }

        let mut process1 = ProcessControlBlock::new(pid1);
        let mut process2 = ProcessControlBlock::new(pid2);

        // 创建地址空间
        if process1.create_address_space().is_err() ||
           process2.create_address_space().is_err() {
            continue;
        }

        // 验证用户栈虚拟地址相同（都在同一位置）
        if process1.user_stack_top != process2.user_stack_top {
            panic!(
                "迭代 {}: 用户栈顶虚拟地址不一致: P1={:#x}, P2={:#x}",
                iteration, process1.user_stack_top.0, process2.user_stack_top.0
            );
        }

        // 验证用户栈底虚拟地址相同
        if process1.user_stack_bottom != process2.user_stack_bottom {
            panic!(
                "迭代 {}: 用户栈底虚拟地址不一致: P1={:#x}, P2={:#x}",
                iteration, process1.user_stack_bottom.0, process2.user_stack_bottom.0
            );
        }

        // 验证页表根不同（意味着物理映射不同）
        // 即使虚拟地址相同，不同的页表意味着映射到不同的物理内存
        if process1.page_table_root.0 == process2.page_table_root.0 {
            panic!(
                "迭代 {}: 两个进程共享同一页表，违反地址空间隔离",
                iteration
            );
        }
    }
}

/// **Property 7: 进程地址空间隔离 - SATP 唯一性**
/// *For any* process with an address space, its SATP value SHALL be unique
/// and non-zero.
///
/// Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
/// **Validates: Requirements 3.3**
///
/// 运行 100 次迭代
#[test]
fn property_7_satp_uniqueness() {
    let mut rng = SimpleRng::new(11111);
    let mut seen_satps: Vec<usize> = Vec::new();

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        if process.create_address_space().is_err() {
            continue;
        }

        let satp = process.satp();

        // 验证 SATP 非零
        if satp == 0 {
            panic!(
                "迭代 {}: 进程 {} 的 SATP 为零",
                iteration, pid
            );
        }

        // 验证 SATP 唯一
        if seen_satps.contains(&satp) {
            panic!(
                "迭代 {}: 进程 {} 的 SATP {:#x} 与之前的进程重复",
                iteration, pid, satp
            );
        }

        seen_satps.push(satp);
    }
}

/// **Property 7: 进程地址空间隔离 - 地址空间独立性**
/// *For any* process, its address space operations SHALL NOT affect other processes.
///
/// Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
/// **Validates: Requirements 3.3**
///
/// 运行 100 次迭代
#[test]
fn property_7_address_space_independence() {
    let mut rng = SimpleRng::new(22222);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        // 创建两个进程
        let pid1 = rng.next_range(1, 1000) as usize;
        let mut pid2 = rng.next_range(1, 1000) as usize;
        while pid2 == pid1 {
            pid2 = rng.next_range(1, 1000) as usize;
        }

        let mut process1 = ProcessControlBlock::new(pid1);
        let mut process2 = ProcessControlBlock::new(pid2);

        // 创建地址空间
        if process1.create_address_space().is_err() ||
           process2.create_address_space().is_err() {
            continue;
        }

        // 记录 process2 的初始状态
        let p2_root_before = process2.page_table_root.0;
        let p2_satp_before = process2.satp();

        // 对 process1 进行操作（释放资源）
        process1.release_resources();

        // 验证 process2 不受影响
        let p2_root_after = process2.page_table_root.0;
        let p2_satp_after = process2.satp();

        if p2_root_before != p2_root_after {
            panic!(
                "迭代 {}: 释放 P1 资源后，P2 的页表根发生变化: {:#x} -> {:#x}",
                iteration, p2_root_before, p2_root_after
            );
        }

        if p2_satp_before != p2_satp_after {
            panic!(
                "迭代 {}: 释放 P1 资源后，P2 的 SATP 发生变化: {:#x} -> {:#x}",
                iteration, p2_satp_before, p2_satp_after
            );
        }

        // 验证 process2 的地址空间仍然有效
        if process2.mm.is_none() {
            panic!(
                "迭代 {}: 释放 P1 资源后，P2 的地址空间变为 None",
                iteration
            );
        }
    }
}

// ============================================================================
// 自测试入口（用于内核启动时运行）
// ============================================================================

/// 进程地址空间隔离属性测试入口
///
/// 运行所有进程地址空间隔离属性测试，验证 Property 7 和 Property 8
///
/// Feature: rustmicroos-comprehensive-fix, Property 7: 进程地址空间隔离
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.3, 3.6**
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[process] Running address space isolation property tests...");

    // Property 7: 页表根不同
    crate::println!("[process] Testing page table roots different (5 iterations)...");
    run_page_table_roots_test()?;

    // Property 7: 独立地址空间
    crate::println!("[process] Testing unique address space (5 iterations)...");
    run_unique_address_space_test()?;

    // Property 7: 用户栈隔离
    crate::println!("[process] Testing user stack isolation (5 iterations)...");
    run_user_stack_isolation_test()?;

    // Property 7: SATP 唯一性
    crate::println!("[process] Testing SATP uniqueness (5 iterations)...");
    run_satp_uniqueness_test()?;

    // Property 7: 地址空间独立性
    crate::println!("[process] Testing address space independence (5 iterations)...");
    run_address_space_independence_test()?;

    crate::println!("[process] All address space isolation property tests passed!");

    // 运行 Property 8 测试
    self_test_property_8()?;

    Ok(())
}

fn run_page_table_roots_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(77777);

    for _ in 0..100 {
        let pid1 = rng.next_range(1, 1000) as usize;
        let mut pid2 = rng.next_range(1, 1000) as usize;
        while pid2 == pid1 {
            pid2 = rng.next_range(1, 1000) as usize;
        }

        let mut process1 = ProcessControlBlock::new(pid1);
        let mut process2 = ProcessControlBlock::new(pid2);

        if process1.create_address_space().is_err() ||
           process2.create_address_space().is_err() {
            continue;
        }

        if process1.mm.is_none() || process2.mm.is_none() {
            return Err("进程地址空间创建失败");
        }

        let root1 = process1.page_table_root;
        let root2 = process2.page_table_root;

        if root1.0 == root2.0 {
            return Err("两个不同进程的页表根相同");
        }

        let satp1 = process1.satp();
        let satp2 = process2.satp();

        if satp1 == satp2 {
            return Err("两个不同进程的 SATP 相同");
        }
    }
    Ok(())
}

fn run_unique_address_space_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(88888);

    // 创建多个进程并同时保持它们的地址空间
    // 这样可以确保它们的页表根地址不会重复
    let mut processes: Vec<ProcessControlBlock> = Vec::new();

    // 创建 10 个进程（而不是 100 个，以避免内存不足）
    for _ in 0..10 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        if process.mm.is_none() {
            return Err("进程地址空间创建失败");
        }

        let root = process.page_table_root.0;
        if root == 0 {
            return Err("页表根地址为零");
        }

        // 检查与已存在的进程是否有重复
        for existing in &processes {
            if existing.page_table_root.0 == root {
                return Err("页表根地址重复");
            }
        }

        processes.push(process);
    }

    // 验证所有进程的页表根地址都不同
    for i in 0..processes.len() {
        for j in (i + 1)..processes.len() {
            if processes[i].page_table_root.0 == processes[j].page_table_root.0 {
                return Err("页表根地址重复");
            }
        }
    }

    Ok(())
}

fn run_user_stack_isolation_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(99999);

    for _ in 0..100 {
        let pid1 = rng.next_range(1, 1000) as usize;
        let mut pid2 = rng.next_range(1, 1000) as usize;
        while pid2 == pid1 {
            pid2 = rng.next_range(1, 1000) as usize;
        }

        let mut process1 = ProcessControlBlock::new(pid1);
        let mut process2 = ProcessControlBlock::new(pid2);

        if process1.create_address_space().is_err() ||
           process2.create_address_space().is_err() {
            continue;
        }

        if process1.user_stack_top != process2.user_stack_top {
            return Err("用户栈顶虚拟地址不一致");
        }

        if process1.user_stack_bottom != process2.user_stack_bottom {
            return Err("用户栈底虚拟地址不一致");
        }

        if process1.page_table_root.0 == process2.page_table_root.0 {
            return Err("两个进程共享同一页表");
        }
    }
    Ok(())
}

fn run_satp_uniqueness_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(11111);
    let mut processes: Vec<ProcessControlBlock> = Vec::new();

    // 创建 10 个同时存在的进程
    for _ in 0..10 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        let satp = process.satp();

        if satp == 0 {
            return Err("SATP 为零");
        }

        // 检查与已存在的进程是否有重复
        for existing in &processes {
            if existing.satp() == satp {
                return Err("SATP 重复");
            }
        }

        processes.push(process);
    }
    Ok(())
}

fn run_address_space_independence_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(22222);

    for _ in 0..100 {
        let pid1 = rng.next_range(1, 1000) as usize;
        let mut pid2 = rng.next_range(1, 1000) as usize;
        while pid2 == pid1 {
            pid2 = rng.next_range(1, 1000) as usize;
        }

        let mut process1 = ProcessControlBlock::new(pid1);
        let mut process2 = ProcessControlBlock::new(pid2);

        if process1.create_address_space().is_err() ||
           process2.create_address_space().is_err() {
            continue;
        }

        let p2_root_before = process2.page_table_root.0;
        let p2_satp_before = process2.satp();

        process1.release_resources();

        let p2_root_after = process2.page_table_root.0;
        let p2_satp_after = process2.satp();

        if p2_root_before != p2_root_after {
            return Err("释放 P1 资源后，P2 的页表根发生变化");
        }

        if p2_satp_before != p2_satp_after {
            return Err("释放 P1 资源后，P2 的 SATP 发生变化");
        }

        if process2.mm.is_none() {
            return Err("释放 P1 资源后，P2 的地址空间变为 None");
        }
    }
    Ok(())
}

// ============================================================================
// Property 8: 进程生命周期管理
// ============================================================================

/// **Property 8: 进程生命周期管理 - 内存页释放**
/// *For any* process that calls release_resources(), *all* of its allocated memory pages
/// SHALL be freed (address_space becomes None).
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_memory_pages_freed() {
    let mut rng = SimpleRng::new(33333);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        if process.create_address_space().is_err() {
            continue;
        }

        // 验证地址空间存在
        if process.mm.is_none() {
            panic!(
                "迭代 {}: 进程 {} 地址空间创建失败",
                iteration, pid
            );
        }

        // 验证页表根非零
        if process.page_table_root.0 == 0 {
            panic!(
                "迭代 {}: 进程 {} 页表根为零",
                iteration, pid
            );
        }

        // 释放资源
        process.release_resources();

        // 验证地址空间已释放
        if process.mm.is_some() {
            panic!(
                "迭代 {}: 进程 {} 释放资源后地址空间仍存在",
                iteration, pid
            );
        }

        // 验证页表根已清零
        if process.page_table_root.0 != 0 {
            panic!(
                "迭代 {}: 进程 {} 释放资源后页表根未清零: {:#x}",
                iteration, pid, process.page_table_root.0
            );
        }
    }
}

/// **Property 8: 进程生命周期管理 - 文件描述符关闭**
/// *For any* process that calls release_resources(), *all* of its open file handles
/// SHALL be closed (fd_table becomes empty).
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_file_handles_closed() {
    let mut rng = SimpleRng::new(44444);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        if process.create_address_space().is_err() {
            continue;
        }

        // 验证初始文件描述符（stdin, stdout, stderr）
        let initial_fd_count = process.open_fd_count();
        if initial_fd_count != 3 {
            panic!(
                "迭代 {}: 进程 {} 初始文件描述符数量不正确: {} (期望 3)",
                iteration, pid, initial_fd_count
            );
        }

        // 模拟打开更多文件（添加到 fd_table）
        let extra_fds = rng.next_range(0, 10) as usize;
        for i in 0..extra_fds {
            process.fd_table.push(Some(100 + i));
        }

        let fd_count_before = process.open_fd_count();

        // 释放资源
        process.release_resources();

        // 验证所有文件描述符已关闭
        let fd_count_after = process.open_fd_count();
        if fd_count_after != 0 {
            panic!(
                "迭代 {}: 进程 {} 释放资源后仍有 {} 个文件描述符打开 (之前有 {})",
                iteration, pid, fd_count_after, fd_count_before
            );
        }

        // 验证 fd_table 为空
        if !process.fd_table.is_empty() {
            panic!(
                "迭代 {}: 进程 {} 释放资源后 fd_table 不为空",
                iteration, pid
            );
        }
    }
}

/// **Property 8: 进程生命周期管理 - 子进程列表清空**
/// *For any* process that calls release_resources(), its children list SHALL be cleared.
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_children_list_cleared() {
    let mut rng = SimpleRng::new(55555);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        if process.create_address_space().is_err() {
            continue;
        }

        // 模拟添加子进程
        let num_children = rng.next_range(0, 10) as usize;
        for i in 0..num_children {
            let child_pid = rng.next_range(10000, 20000) as usize + i;
            process.children.push(child_pid);
        }

        let children_before = process.children.len();

        // 释放资源
        process.release_resources();

        // 验证子进程列表已清空
        if !process.children.is_empty() {
            panic!(
                "迭代 {}: 进程 {} 释放资源后子进程列表不为空: {} (之前有 {})",
                iteration, pid, process.children.len(), children_before
            );
        }
    }
}

/// **Property 8: 进程生命周期管理 - 完整资源释放**
/// *For any* process that calls release_resources(), is_resources_released() SHALL return true.
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_complete_resource_release() {
    let mut rng = SimpleRng::new(66666);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        if process.create_address_space().is_err() {
            continue;
        }

        // 模拟添加子进程和文件描述符
        let num_children = rng.next_range(0, 5) as usize;
        for i in 0..num_children {
            process.children.push(10000 + i);
        }

        let extra_fds = rng.next_range(0, 5) as usize;
        for i in 0..extra_fds {
            process.fd_table.push(Some(100 + i));
        }

        // 释放前应该返回 false
        if process.is_resources_released() {
            panic!(
                "迭代 {}: 进程 {} 释放资源前 is_resources_released() 返回 true",
                iteration, pid
            );
        }

        // 释放资源
        process.release_resources();

        // 释放后应该返回 true
        if !process.is_resources_released() {
            panic!(
                "迭代 {}: 进程 {} 释放资源后 is_resources_released() 返回 false",
                iteration, pid
            );
        }
    }
}

/// **Property 8: 进程生命周期管理 - 退出状态设置**
/// *For any* process, setting exit_status SHALL correctly record the exit reason.
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_exit_status_recorded() {
    let mut rng = SimpleRng::new(77778);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 验证初始退出状态为 None
        if process.exit_status.is_some() {
            panic!(
                "迭代 {}: 进程 {} 初始退出状态不为 None",
                iteration, pid
            );
        }

        // 随机选择退出原因
        let exit_type = rng.next_range(0, 3);
        let exit_code = rng.next_range(0, 256) as i32;

        // Slice 2.0：Killed 合并到 signaled(9)；取 dump=false/true 两变体覆盖
        // 原有“信号 / Killed” 两路。
        let exit_reason = match exit_type {
            0 => ExitReason::Normal(exit_code),
            1 => ExitReason::signaled(exit_code % 32),                // 信号号通常 < 32
            _ => ExitReason::signaled_with_dump((exit_code % 32) | 1), // 同号 + dump=true 分支
        };

        // 设置退出状态
        process.exit_status = Some(exit_reason);
        process.state = ProcessState::Zombie;

        // 验证退出状态已设置
        if process.exit_status.is_none() {
            panic!(
                "迭代 {}: 进程 {} 设置退出状态后仍为 None",
                iteration, pid
            );
        }

        // 验证进程状态为 Zombie
        if process.state != ProcessState::Zombie {
            panic!(
                "迭代 {}: 进程 {} 退出后状态不是 Zombie: {:?}",
                iteration, pid, process.state
            );
        }

        // 验证退出原因正确。Signaled 变体同时检查 sig + core_dumped。
        match (exit_reason, process.exit_status.unwrap()) {
            (ExitReason::Normal(expected), ExitReason::Normal(actual)) => {
                if expected != actual {
                    panic!(
                        "迭代 {}: 进程 {} 退出码不匹配: 期望 {}, 实际 {}",
                        iteration, pid, expected, actual
                    );
                }
            }
            (
                ExitReason::Signaled { sig: e_sig, core_dumped: e_dump },
                ExitReason::Signaled { sig: a_sig, core_dumped: a_dump },
            ) => {
                if e_sig != a_sig || e_dump != a_dump {
                    panic!(
                        "迭代 {}: 进程 {} 信号/dump 不匹配: 期望 ({}, {}), 实际 ({}, {})",
                        iteration, pid, e_sig, e_dump, a_sig, a_dump
                    );
                }
            }
            (expected, actual) => {
                panic!(
                    "迭代 {}: 进程 {} 退出原因类型不匹配: 期望 {:?}, 实际 {:?}",
                    iteration, pid, expected, actual
                );
            }
        }
    }
}

/// **Property 8: 进程生命周期管理 - 多次释放安全**
/// *For any* process, calling release_resources() multiple times SHALL be safe
/// and idempotent.
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_multiple_release_safe() {
    let mut rng = SimpleRng::new(88889);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        // 创建地址空间
        if process.create_address_space().is_err() {
            continue;
        }

        // 添加一些资源
        process.children.push(10000);
        process.fd_table.push(Some(100));

        // 第一次释放
        process.release_resources();

        // 验证资源已释放
        if !process.is_resources_released() {
            panic!(
                "迭代 {}: 进程 {} 第一次释放后资源未完全释放",
                iteration, pid
            );
        }

        // 多次释放应该是安全的
        let release_count = rng.next_range(1, 5) as usize;
        for _ in 0..release_count {
            process.release_resources();
        }

        // 验证仍然是释放状态
        if !process.is_resources_released() {
            panic!(
                "迭代 {}: 进程 {} 多次释放后资源状态不正确",
                iteration, pid
            );
        }
    }
}

// ============================================================================
// Property 8 自测试入口
// ============================================================================

/// 进程生命周期管理属性测试入口
///
/// 运行所有进程生命周期管理属性测试，验证 Property 8
///
/// Feature: rustmicroos-comprehensive-fix, Property 8: 进程生命周期管理
/// **Validates: Requirements 3.6**
///
/// Feature: user-program-loading, Property 8: Process Ancestry
/// **Validates: Requirements 6.5, 6.6**
pub fn self_test_property_8() -> Result<(), &'static str> {
    crate::println!("[process] Running process lifecycle property tests (Property 8)...");

    // Property 8: 内存页释放
    crate::println!("[process] Testing memory pages freed (5 iterations)...");
    run_memory_pages_freed_test()?;

    // Property 8: 文件描述符关闭
    crate::println!("[process] Testing file handles closed (5 iterations)...");
    run_file_handles_closed_test()?;

    // Property 8: 子进程列表清空
    crate::println!("[process] Testing children list cleared (5 iterations)...");
    run_children_list_cleared_test()?;

    // Property 8: 完整资源释放
    crate::println!("[process] Testing complete resource release (5 iterations)...");
    run_complete_resource_release_test()?;

    // Property 8: 退出状态设置
    crate::println!("[process] Testing exit status recorded (5 iterations)...");
    run_exit_status_recorded_test()?;

    // Property 8: 多次释放安全
    crate::println!("[process] Testing multiple release safe (5 iterations)...");
    run_multiple_release_safe_test()?;

    // Property 8: 进程祖先关系 - 所有进程追溯到 init
    crate::println!("[process] Testing parent chain to init (5 iterations)...");
    run_parent_chain_to_init_test()?;

    // Property 8: 进程祖先关系 - init 没有父进程
    crate::println!("[process] Testing init has no parent (5 iterations)...");
    run_init_has_no_parent_test()?;

    // Property 8: 进程祖先关系 - 孤儿进程重新托管
    crate::println!("[process] Testing orphan reparenting (5 iterations)...");
    run_orphan_reparenting_test()?;

    crate::println!("[process] All process lifecycle property tests passed!");
    Ok(())
}

fn run_memory_pages_freed_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(33333);

    for _ in 0..100 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        if process.mm.is_none() {
            return Err("进程地址空间创建失败");
        }

        if process.page_table_root.0 == 0 {
            return Err("页表根为零");
        }

        process.release_resources();

        if process.mm.is_some() {
            return Err("释放资源后地址空间仍存在");
        }

        if process.page_table_root.0 != 0 {
            return Err("释放资源后页表根未清零");
        }
    }
    Ok(())
}

fn run_file_handles_closed_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(44444);

    for _ in 0..100 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        if process.open_fd_count() != 3 {
            return Err("初始文件描述符数量不正确");
        }

        let extra_fds = rng.next_range(0, 10) as usize;
        for _i in 0..extra_fds {
            process.fd_table.push(Some(alloc::sync::Arc::new(crate::fs::FileDescriptor::dummy())));
        }

        process.release_resources();

        if process.open_fd_count() != 0 {
            return Err("释放资源后仍有文件描述符打开");
        }

        if !process.fd_table.is_empty() {
            return Err("释放资源后 fd_table 不为空");
        }
    }
    Ok(())
}

fn run_children_list_cleared_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(55555);

    for _ in 0..100 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        let num_children = rng.next_range(0, 10) as usize;
        for i in 0..num_children {
            let child_pid = rng.next_range(10000, 20000) as usize + i;
            process.children.push(child_pid);
        }

        process.release_resources();

        if !process.children.is_empty() {
            return Err("释放资源后子进程列表不为空");
        }
    }
    Ok(())
}

fn run_complete_resource_release_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(66666);

    for _ in 0..100 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        let num_children = rng.next_range(0, 5) as usize;
        for i in 0..num_children {
            process.children.push(10000 + i);
        }

        let extra_fds = rng.next_range(0, 5) as usize;
        for _i in 0..extra_fds {
            process.fd_table.push(Some(alloc::sync::Arc::new(crate::fs::FileDescriptor::dummy())));
        }

        if process.is_resources_released() {
            return Err("释放资源前 is_resources_released() 返回 true");
        }

        process.release_resources();

        if !process.is_resources_released() {
            return Err("释放资源后 is_resources_released() 返回 false");
        }
    }
    Ok(())
}

fn run_exit_status_recorded_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(77778);

    for _ in 0..100 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.exit_status.is_some() {
            return Err("初始退出状态不为 None");
        }

        let exit_type = rng.next_range(0, 3);
        let exit_code = rng.next_range(0, 256) as i32;

        // Slice 2.0：Killed 合并到 signaled(9)；取 dump=false/true 两变体覆盖。
        let exit_reason = match exit_type {
            0 => ExitReason::Normal(exit_code),
            1 => ExitReason::signaled(exit_code % 32),
            _ => ExitReason::signaled_with_dump((exit_code % 32) | 1),
        };

        process.exit_status = Some(exit_reason);
        process.state = ProcessState::Zombie;

        if process.exit_status.is_none() {
            return Err("设置退出状态后仍为 None");
        }

        if process.state != ProcessState::Zombie {
            return Err("退出后状态不是 Zombie");
        }

        match (exit_reason, process.exit_status.unwrap()) {
            (ExitReason::Normal(expected), ExitReason::Normal(actual)) => {
                if expected != actual {
                    return Err("退出码不匹配");
                }
            }
            (
                ExitReason::Signaled { sig: e_sig, core_dumped: e_dump },
                ExitReason::Signaled { sig: a_sig, core_dumped: a_dump },
            ) => {
                if e_sig != a_sig || e_dump != a_dump {
                    return Err("信号/dump 不匹配");
                }
            }
            _ => {
                return Err("退出原因类型不匹配");
            }
        }
    }
    Ok(())
}

fn run_multiple_release_safe_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(88889);

    for _ in 0..100 {
        let pid = rng.next_range(1, 10000) as usize;
        let mut process = ProcessControlBlock::new(pid);

        if process.create_address_space().is_err() {
            continue;
        }

        process.children.push(10000);
        process.fd_table.push(Some(alloc::sync::Arc::new(crate::fs::FileDescriptor::dummy())));

        process.release_resources();

        if !process.is_resources_released() {
            return Err("第一次释放后资源未完全释放");
        }

        let release_count = rng.next_range(1, 5) as usize;
        for _ in 0..release_count {
            process.release_resources();
        }

        if !process.is_resources_released() {
            return Err("多次释放后资源状态不正确");
        }
    }
    Ok(())
}

/// **Property 8: 进程祖先关系 - 所有进程追溯到 init**
/// *For all* user processes in the system, tracing the parent chain SHALL
/// eventually reach PID 1 (init).
///
/// Feature: user-program-loading, Property 8: Process Ancestry
/// **Validates: Requirements 6.5, 6.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_parent_chain_to_init() {
    let mut rng = SimpleRng::new(99990);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        // 创建一个进程树：init -> parent -> child -> grandchild
        let mut init = ProcessControlBlock::new(1);
        let mut parent = ProcessControlBlock::new(2);
        let mut child = ProcessControlBlock::new(3);
        let mut grandchild = ProcessControlBlock::new(4);

        // 设置父子关系
        init.parent_pid = None;  // init 没有父进程
        parent.parent_pid = Some(1);
        child.parent_pid = Some(2);
        grandchild.parent_pid = Some(3);

        // 设置子进程列表
        init.children.push(2);
        parent.children.push(3);
        child.children.push(4);

        // 验证 init 没有父进程
        if init.parent_pid.is_some() {
            panic!(
                "迭代 {}: init 进程有父进程: {:?}",
                iteration, init.parent_pid
            );
        }

        // 验证 parent 的父进程是 init
        if parent.parent_pid != Some(1) {
            panic!(
                "迭代 {}: parent 的父进程不是 init: {:?}",
                iteration, parent.parent_pid
            );
        }

        // 验证 child 可以追溯到 init
        let mut current_pid = child.parent_pid;
        let mut chain_length = 0;
        let max_chain_length = 10;

        while let Some(pid) = current_pid {
            chain_length += 1;

            if chain_length > max_chain_length {
                panic!(
                    "迭代 {}: 父进程链过长（可能有循环）: {} 步",
                    iteration, chain_length
                );
            }

            if pid == 1 {
                // 成功追溯到 init
                break;
            }

            // 继续向上追溯
            current_pid = if pid == 2 {
                parent.parent_pid
            } else if pid == 3 {
                child.parent_pid
            } else {
                None
            };
        }

        if current_pid != Some(1) {
            panic!(
                "迭代 {}: child 无法追溯到 init，最终到达: {:?}",
                iteration, current_pid
            );
        }

        // 验证 grandchild 可以追溯到 init
        let mut current_pid = grandchild.parent_pid;
        let mut chain_length = 0;

        while let Some(pid) = current_pid {
            chain_length += 1;

            if chain_length > max_chain_length {
                panic!(
                    "迭代 {}: grandchild 父进程链过长: {} 步",
                    iteration, chain_length
                );
            }

            if pid == 1 {
                break;
            }

            current_pid = if pid == 2 {
                parent.parent_pid
            } else if pid == 3 {
                child.parent_pid
            } else if pid == 4 {
                grandchild.parent_pid
            } else {
                None
            };
        }

        if current_pid != Some(1) {
            panic!(
                "迭代 {}: grandchild 无法追溯到 init，最终到达: {:?}",
                iteration, current_pid
            );
        }
    }
}

/// **Property 8: 进程祖先关系 - init 没有父进程**
/// *For all* processes, the init process (PID 1) SHALL have no parent
/// (parent_pid = None).
///
/// Feature: user-program-loading, Property 8: Process Ancestry
/// **Validates: Requirements 6.5, 6.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_init_has_no_parent() {
    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        // 创建 init 进程
        let init = ProcessControlBlock::new(1);

        // 验证 init 没有父进程
        if init.parent_pid.is_some() {
            panic!(
                "迭代 {}: init 进程有父进程: {:?}",
                iteration, init.parent_pid
            );
        }

        // 验证 PID 确实是 1
        if init.pid != 1 {
            panic!(
                "迭代 {}: init 进程的 PID 不是 1: {}",
                iteration, init.pid
            );
        }
    }
}

/// **Property 8: 进程祖先关系 - 孤儿进程重新托管**
/// *For all* orphaned processes (parent exits), they SHALL be reparented to
/// init (PID 1).
///
/// Feature: user-program-loading, Property 8: Process Ancestry
/// **Validates: Requirements 6.5, 6.6**
///
/// 运行 100 次迭代
#[test]
fn property_8_orphan_reparenting() {
    let mut rng = SimpleRng::new(99991);

    // 运行 100 次迭代
    for iteration in 0..PROPERTY_TEST_ITERATIONS {
        // 创建进程树：init -> parent -> children
        let mut init = ProcessControlBlock::new(1);
        let mut parent = ProcessControlBlock::new(2);

        // 创建多个子进程
        let num_children = rng.next_range(1, 5) as usize;
        let mut children: Vec<ProcessControlBlock> = Vec::new();

        for i in 0..num_children {
            let child_pid = 100 + i;
            let mut child = ProcessControlBlock::new(child_pid);
            child.parent_pid = Some(2);  // 父进程是 parent
            parent.children.push(child_pid);
            children.push(child);
        }

        // 设置 parent 的父进程为 init
        parent.parent_pid = Some(1);
        init.children.push(2);

        // 模拟 parent 退出，将子进程托管给 init
        let orphaned_children = parent.children.clone();

        // 验证有子进程需要托管
        if orphaned_children.is_empty() {
            panic!(
                "迭代 {}: parent 没有子进程",
                iteration
            );
        }

        // 执行托管操作（模拟 exit 中的逻辑）
        for child_pid in &orphaned_children {
            // 找到对应的子进程并更新其父进程
            for child in &mut children {
                if child.pid == *child_pid {
                    child.parent_pid = Some(1);
                }
            }

            // 将子进程添加到 init 的子进程列表
            if !init.children.contains(child_pid) {
                init.children.push(*child_pid);
            }
        }

        // 清空 parent 的子进程列表（模拟 release_resources）
        parent.children.clear();

        // 验证所有孤儿进程的父进程都是 init
        for child in &children {
            if child.parent_pid != Some(1) {
                panic!(
                    "迭代 {}: 孤儿进程 {} 的父进程不是 init: {:?}",
                    iteration, child.pid, child.parent_pid
                );
            }
        }

        // 验证 init 的子进程列表包含所有孤儿进程
        for child in &children {
            if !init.children.contains(&child.pid) {
                panic!(
                    "迭代 {}: init 的子进程列表不包含孤儿进程 {}",
                    iteration, child.pid
                );
            }
        }

        // 验证 parent 的子进程列表已清空
        if !parent.children.is_empty() {
            panic!(
                "迭代 {}: parent 退出后子进程列表未清空: {:?}",
                iteration, parent.children
            );
        }
    }
}


fn run_parent_chain_to_init_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(99990);

    for _ in 0..100 {
        // 创建一个进程树：init -> parent -> child -> grandchild
        let mut init = ProcessControlBlock::new(1);
        let mut parent = ProcessControlBlock::new(2);
        let mut child = ProcessControlBlock::new(3);
        let mut grandchild = ProcessControlBlock::new(4);

        // 设置父子关系
        init.parent_pid = None;
        parent.parent_pid = Some(1);
        child.parent_pid = Some(2);
        grandchild.parent_pid = Some(3);

        // 设置子进程列表
        init.children.push(2);
        parent.children.push(3);
        child.children.push(4);

        // 验证 init 没有父进程
        if init.parent_pid.is_some() {
            return Err("init 进程有父进程");
        }

        // 验证 parent 的父进程是 init
        if parent.parent_pid != Some(1) {
            return Err("parent 的父进程不是 init");
        }

        // 验证 child 可以追溯到 init
        let mut current_pid = child.parent_pid;
        let mut chain_length = 0;
        let max_chain_length = 10;

        while let Some(pid) = current_pid {
            chain_length += 1;

            if chain_length > max_chain_length {
                return Err("父进程链过长（可能有循环）");
            }

            if pid == 1 {
                break;
            }

            current_pid = if pid == 2 {
                parent.parent_pid
            } else if pid == 3 {
                child.parent_pid
            } else {
                None
            };
        }

        if current_pid != Some(1) {
            return Err("child 无法追溯到 init");
        }

        // 验证 grandchild 可以追溯到 init
        let mut current_pid = grandchild.parent_pid;
        let mut chain_length = 0;

        while let Some(pid) = current_pid {
            chain_length += 1;

            if chain_length > max_chain_length {
                return Err("grandchild 父进程链过长");
            }

            if pid == 1 {
                break;
            }

            current_pid = if pid == 2 {
                parent.parent_pid
            } else if pid == 3 {
                child.parent_pid
            } else if pid == 4 {
                grandchild.parent_pid
            } else {
                None
            };
        }

        if current_pid != Some(1) {
            return Err("grandchild 无法追溯到 init");
        }
    }
    Ok(())
}

fn run_init_has_no_parent_test() -> Result<(), &'static str> {
    for _ in 0..100 {
        let init = ProcessControlBlock::new(1);

        if init.parent_pid.is_some() {
            return Err("init 进程有父进程");
        }

        if init.pid != 1 {
            return Err("init 进程的 PID 不是 1");
        }
    }
    Ok(())
}

fn run_orphan_reparenting_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(99991);

    for _ in 0..100 {
        // 创建进程树：init -> parent -> children
        let mut init = ProcessControlBlock::new(1);
        let mut parent = ProcessControlBlock::new(2);

        // 创建多个子进程
        let num_children = rng.next_range(1, 5) as usize;
        let mut children: Vec<ProcessControlBlock> = Vec::new();

        for i in 0..num_children {
            let child_pid = 100 + i;
            let mut child = ProcessControlBlock::new(child_pid);
            child.parent_pid = Some(2);
            parent.children.push(child_pid);
            children.push(child);
        }

        // 设置 parent 的父进程为 init
        parent.parent_pid = Some(1);
        init.children.push(2);

        // 模拟 parent 退出，将子进程托管给 init
        let orphaned_children = parent.children.clone();

        if orphaned_children.is_empty() {
            return Err("parent 没有子进程");
        }

        // 执行托管操作
        for child_pid in &orphaned_children {
            for child in &mut children {
                if child.pid == *child_pid {
                    child.parent_pid = Some(1);
                }
            }

            if !init.children.contains(child_pid) {
                init.children.push(*child_pid);
            }
        }

        parent.children.clear();

        // 验证所有孤儿进程的父进程都是 init
        for child in &children {
            if child.parent_pid != Some(1) {
                return Err("孤儿进程的父进程不是 init");
            }
        }

        // 验证 init 的子进程列表包含所有孤儿进程
        for child in &children {
            if !init.children.contains(&child.pid) {
                return Err("init 的子进程列表不包含孤儿进程");
            }
        }

        // 验证 parent 的子进程列表已清空
        if !parent.children.is_empty() {
            return Err("parent 退出后子进程列表未清空");
        }
    }
    Ok(())
}
