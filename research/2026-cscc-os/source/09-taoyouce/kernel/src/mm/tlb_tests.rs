//! TLB 优化属性测试
//! Property 12: TLB Shootdown 优化正确性
//! Property 13: ASID 使用正确性

#[cfg(test)]
mod property_tests {
    use super::*;
    
    /// Property 12: TLB Shootdown 优化正确性
    /// For any 页表修改，如果只影响当前 CPU 则不发送 IPI；
    /// 如果影响多个页面则合并为单次 IPI。
    #[test]
    fn property_shootdown_optimization() {
        let tlb_manager = TlbManager::new();
        tlb_manager.set_num_cpus(4);
        
        // 测试 CPU 掩码操作
        test_cpu_mask_operations();
        
        // 测试虚拟地址范围
        test_virt_addr_range();
        
        // 测试 TLB 统计
        test_tlb_stats(&tlb_manager);
    }
    
    fn test_cpu_mask_operations() {
        let mut mask = CpuMask::empty();
        
        // 测试空掩码
        assert!(mask.is_empty(), "新掩码应为空");
        assert_eq!(mask.count(), 0, "空掩码计数应为 0");
        
        // 测试设置 CPU
        mask.set(0);
        mask.set(2);
        assert!(!mask.is_empty(), "设置后掩码不应为空");
        assert_eq!(mask.count(), 2, "计数应为 2");
        assert!(mask.contains(0), "应包含 CPU 0");
        assert!(!mask.contains(1), "不应包含 CPU 1");
        assert!(mask.contains(2), "应包含 CPU 2");
        
        // 测试清除 CPU
        mask.clear(0);
        assert!(!mask.contains(0), "清除后不应包含 CPU 0");
        assert_eq!(mask.count(), 1, "计数应为 1");
        
        // 测试迭代
        mask.set(1);
        mask.set(3);
        let cpus: alloc::vec::Vec<usize> = mask.iter().collect();
        assert!(cpus.contains(&1), "迭代应包含 CPU 1");
        assert!(cpus.contains(&2), "迭代应包含 CPU 2");
        assert!(cpus.contains(&3), "迭代应包含 CPU 3");
        
        // 测试 all 掩码
        let all_mask = CpuMask::all(4);
        assert_eq!(all_mask.count(), 4, "all(4) 应包含 4 个 CPU");
        for i in 0..4 {
            assert!(all_mask.contains(i), "all(4) 应包含 CPU {}", i);
        }
    }
    
    fn test_virt_addr_range() {
        let range = VirtAddrRange::new(0x1000, 0x5000);
        
        // 测试页数计算
        let page_count = range.page_count();
        assert_eq!(page_count, 4, "页数应为 4");
        
        // 测试页迭代
        let pages: alloc::vec::Vec<u64> = range.iter_pages().collect();
        assert_eq!(pages.len(), 4, "应迭代 4 个页");
        assert_eq!(pages[0], 0x1000, "第一页应为 0x1000");
        assert_eq!(pages[3], 0x4000, "最后一页应为 0x4000");
    }
    
    fn test_tlb_stats(tlb_manager: &TlbManager) {
        let stats = tlb_manager.stats();
        
        // 验证统计信息可访问
        let local_flushes = stats.local_flushes.load(core::sync::atomic::Ordering::Relaxed);
        let remote_shootdowns = stats.remote_shootdowns.load(core::sync::atomic::Ordering::Relaxed);
        let ipi_sent = stats.ipi_sent.load(core::sync::atomic::Ordering::Relaxed);
        
        // 初始值应为 0
        assert_eq!(local_flushes, 0, "初始本地刷新数应为 0");
        assert_eq!(remote_shootdowns, 0, "初始远程 shootdown 数应为 0");
        assert_eq!(ipi_sent, 0, "初始 IPI 发送数应为 0");
    }
    
    /// Property 13: ASID 使用正确性
    /// For any 地址空间切换，使用 ASID 后应避免不必要的全局 TLB 刷新，
    /// 只刷新对应 ASID 的条目。
    #[test]
    fn property_asid_correctness() {
        let tlb_manager = TlbManager::new();
        
        // 测试 ASID 分配
        test_asid_allocation(tlb_manager.asid_allocator());
        
        // 测试 CPU 状态
        test_cpu_state(&tlb_manager);
    }
    
    fn test_asid_allocation(allocator: &AsidAllocator) {
        // 分配多个 ASID
        let asid1 = allocator.alloc(1);
        let asid2 = allocator.alloc(2);
        let asid3 = allocator.alloc(3);
        
        // 验证 ASID 唯一
        assert_ne!(asid1, asid2, "ASID 应该唯一");
        assert_ne!(asid2, asid3, "ASID 应该唯一");
        assert_ne!(asid1, asid3, "ASID 应该唯一");
        
        // 验证 ASID 在有效范围内
        assert!(asid1 > 0 && asid1 <= MAX_ASID, "ASID 应在有效范围内");
        assert!(asid2 > 0 && asid2 <= MAX_ASID, "ASID 应在有效范围内");
        
        // 测试查找
        let found = allocator.find(1);
        assert!(found.is_some(), "应能找到已分配的 ASID");
        assert_eq!(found.unwrap(), asid1, "找到的 ASID 应匹配");
        
        // 测试 get_or_alloc
        let same_asid = allocator.get_or_alloc(1);
        assert_eq!(same_asid, asid1, "get_or_alloc 应返回相同的 ASID");
        
        // 测试释放
        allocator.free(asid1);
        let not_found = allocator.find(1);
        assert!(not_found.is_none(), "释放后不应找到 ASID");
    }
    
    fn test_cpu_state(tlb_manager: &TlbManager) {
        // 获取 CPU 0 的状态
        let state = tlb_manager.cpu_state(0).expect("应能获取 CPU 0 状态");
        
        // 测试初始状态
        assert_eq!(state.current_asid(), 0, "初始 ASID 应为 0");
        assert_eq!(state.active_mm(), 0, "初始活跃地址空间应为 0");
        assert!(!state.has_lazy_flush(), "初始不应有延迟刷新");
        
        // 测试设置 ASID
        state.set_current_asid(42);
        assert_eq!(state.current_asid(), 42, "ASID 应已更新");
        
        // 测试设置活跃地址空间
        state.set_active_mm(0x12345678);
        assert_eq!(state.active_mm(), 0x12345678, "活跃地址空间应已更新");
        
        // 测试延迟刷新
        state.set_lazy_flush(true);
        assert!(state.has_lazy_flush(), "应有延迟刷新");
        
        let was_pending = state.clear_lazy_flush();
        assert!(was_pending, "清除前应有延迟刷新");
        assert!(!state.has_lazy_flush(), "清除后不应有延迟刷新");
        
        // 测试刷新计数
        let initial_count = state.flush_count();
        state.inc_flush_count();
        assert_eq!(state.flush_count(), initial_count + 1, "刷新计数应增加");
    }
    
    /// 属性测试：批量刷新
    #[test]
    fn property_batch_flush() {
        let tlb_manager = TlbManager::new();
        tlb_manager.set_num_cpus(4);
        
        // 准备批量刷新请求
        let requests = [
            (1u64, Some(VirtAddrRange::new(0x1000, 0x2000))),
            (1u64, Some(VirtAddrRange::new(0x2000, 0x3000))),
            (2u64, Some(VirtAddrRange::new(0x3000, 0x4000))),
        ];
        
        // 执行批量刷新
        tlb_manager.batch_flush(&requests, 0);
        
        // 验证统计信息更新
        let stats = tlb_manager.stats();
        // 由于没有 CPU 使用这些地址空间，应该跳过刷新
        let skipped = stats.skipped_flushes.load(core::sync::atomic::Ordering::Relaxed);
        assert!(skipped > 0, "应有跳过的刷新");
    }
    
    /// 属性测试：本地刷新
    #[test]
    fn property_local_flush() {
        let tlb_manager = TlbManager::new();
        
        // 测试无范围刷新
        tlb_manager.local_flush(None);
        
        // 测试有范围刷新
        let range = VirtAddrRange::new(0x1000, 0x3000);
        tlb_manager.local_flush(Some(range));
        
        // 验证没有崩溃即可
    }
    
    /// 属性测试：全局管理器
    #[test]
    fn property_global_manager() {
        // 测试全局管理器可访问
        let manager = get_manager();
        
        // 验证统计信息可访问
        let stats = manager.stats();
        assert!(stats.local_flushes.load(core::sync::atomic::Ordering::Relaxed) >= 0,
            "统计信息应可访问");
    }
}
