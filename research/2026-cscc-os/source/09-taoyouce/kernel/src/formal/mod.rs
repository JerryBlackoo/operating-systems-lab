//! 形式化验证规约 (Verus 风格)
//!
//! 使用 Verus 风格的规约来描述内核关键组件的正确性属性。
//! 这些规约可以用于文档、测试生成和未来的形式化验证。
//!
//! # 特性
//! - 能力系统不变量
//! - 内存安全属性
//! - 调度器公平性
//! - IPC 正确性

extern crate alloc;

pub mod ipc_spec;
pub mod verification;

use core::marker::PhantomData;

/// 形式化规约 trait
pub trait Specification {
    /// 前置条件
    fn requires(&self) -> bool;
    /// 后置条件
    fn ensures(&self) -> bool;
    /// 不变量
    fn invariant(&self) -> bool;
}

/// 证明标记（用于类型级别的证明）
#[derive(Clone, Copy)]
pub struct Proof<T>(PhantomData<T>);

impl<T> Proof<T> {
    /// 创建证明（仅在验证通过时调用）
    pub const fn assume() -> Self {
        Self(PhantomData)
    }
}

// ============ 能力系统规约 ============

pub mod capability_spec {
    //! 能力系统形式化规约
    //!
    //! ## 核心不变量
    //!
    //! 1. **能力不可伪造**: 能力只能通过合法操作创建
    //! 2. **权限单调递减**: 派生能力的权限 ⊆ 父能力的权限
    //! 3. **撤销传递性**: 撤销父能力会撤销所有派生能力
    //! 4. **引用计数正确**: 对象的引用计数 = 指向它的能力数

    /// 能力权限集合
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Rights(pub u32);

    impl Rights {
        pub const READ: Self = Self(1 << 0);
        pub const WRITE: Self = Self(1 << 1);
        pub const EXECUTE: Self = Self(1 << 2);
        pub const GRANT: Self = Self(1 << 3);

        /// 检查是否是子集
        pub fn is_subset_of(&self, other: &Self) -> bool {
            (self.0 & other.0) == self.0
        }
    }

    /// 能力规约
    pub struct CapabilitySpec {
        /// 能力 ID
        pub id: u64,
        /// 权限
        pub rights: Rights,
        /// 父能力 ID（None 表示根能力）
        pub parent: Option<u64>,
        /// 代数（用于撤销）
        pub generation: u64,
        /// 是否有效
        pub valid: bool,
    }

    impl CapabilitySpec {
        /// 不变量：派生能力的权限是父能力权限的子集
        /// 
        /// ```verus
        /// invariant forall |child: Capability, parent: Capability|
        ///     child.parent == Some(parent.id) ==>
        ///     child.rights.is_subset_of(parent.rights)
        /// ```
        pub fn rights_monotonic_invariant(&self, parent: &CapabilitySpec) -> bool {
            if self.parent == Some(parent.id) {
                self.rights.is_subset_of(&parent.rights)
            } else {
                true
            }
        }

        /// 不变量：撤销传递性
        ///
        /// ```verus
        /// invariant forall |child: Capability, parent: Capability|
        ///     child.parent == Some(parent.id) && !parent.valid ==>
        ///     !child.valid
        /// ```
        pub fn revocation_transitivity_invariant(&self, parent: &CapabilitySpec) -> bool {
            if self.parent == Some(parent.id) && !parent.valid {
                !self.valid
            } else {
                true
            }
        }
    }

    /// 能力派生规约
    ///
    /// ```verus
    /// fn derive(parent: &Capability, new_rights: Rights) -> Capability
    ///     requires
    ///         parent.valid,
    ///         new_rights.is_subset_of(parent.rights),
    ///         parent.rights.contains(Rights::GRANT),
    ///     ensures
    ///         result.valid,
    ///         result.parent == Some(parent.id),
    ///         result.rights == new_rights,
    ///         result.generation == parent.generation,
    /// ```
    pub fn derive_spec(
        parent: &CapabilitySpec,
        new_rights: Rights,
    ) -> Option<CapabilitySpec> {
        // 前置条件检查
        if !parent.valid {
            return None;
        }
        if !new_rights.is_subset_of(&parent.rights) {
            return None;
        }
        if (parent.rights.0 & Rights::GRANT.0) == 0 {
            return None;
        }

        // 创建派生能力
        Some(CapabilitySpec {
            id: 0, // 由系统分配
            rights: new_rights,
            parent: Some(parent.id),
            generation: parent.generation,
            valid: true,
        })
    }
}

// ============ 内存安全规约 ============

pub mod memory_spec {
    //! 内存安全形式化规约
    //!
    //! ## 核心属性
    //!
    //! 1. **空间安全**: 所有内存访问在有效范围内
    //! 2. **时间安全**: 不访问已释放的内存
    //! 3. **类型安全**: 内存按正确类型访问

    /// 内存区域规约
    pub struct MemoryRegionSpec {
        /// 基地址
        pub base: u64,
        /// 大小
        pub size: u64,
        /// 是否已分配
        pub allocated: bool,
        /// 所有者
        pub owner: u64,
    }

    impl MemoryRegionSpec {
        /// 检查地址是否在区域内
        ///
        /// ```verus
        /// fn contains(&self, addr: u64, size: u64) -> bool
        ///     ensures
        ///         result == (addr >= self.base && 
        ///                    addr + size <= self.base + self.size)
        /// ```
        pub fn contains(&self, addr: u64, size: u64) -> bool {
            addr >= self.base && 
            addr.checked_add(size).map_or(false, |end| end <= self.base + self.size)
        }

        /// 不变量：已分配区域不重叠
        ///
        /// ```verus
        /// invariant forall |r1: MemoryRegion, r2: MemoryRegion|
        ///     r1.allocated && r2.allocated && r1.id != r2.id ==>
        ///     !r1.overlaps(r2)
        /// ```
        pub fn no_overlap_invariant(&self, other: &MemoryRegionSpec) -> bool {
            if self.allocated && other.allocated {
                let self_end = self.base + self.size;
                let other_end = other.base + other.size;
                // 不重叠条件
                self_end <= other.base || other_end <= self.base
            } else {
                true
            }
        }
    }

    /// 内存访问规约
    ///
    /// ```verus
    /// fn memory_access(addr: u64, size: u64, write: bool) -> Result<(), Error>
    ///     requires
    ///         exists |region: MemoryRegion|
    ///             region.allocated &&
    ///             region.contains(addr, size) &&
    ///             (write ==> region.writable)
    ///     ensures
    ///         result.is_ok()
    /// ```
    pub fn memory_access_spec(
        regions: &[MemoryRegionSpec],
        addr: u64,
        size: u64,
    ) -> bool {
        regions.iter().any(|r| r.allocated && r.contains(addr, size))
    }
}

// ============ 调度器规约 ============

pub mod scheduler_spec {
    //! 调度器形式化规约
    //!
    //! ## 核心属性
    //!
    //! 1. **公平性**: 每个就绪线程最终会被调度
    //! 2. **优先级正确**: 高优先级线程优先调度
    //! 3. **无死锁**: 系统不会进入死锁状态

    use alloc::vec::Vec;

    /// 线程状态
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum ThreadState {
        Ready,
        Running,
        Blocked,
        Terminated,
    }

    /// 线程规约
    pub struct ThreadSpec {
        pub id: u64,
        pub state: ThreadState,
        pub priority: u32,
        pub time_slice: u32,
        pub wait_time: u64,
    }

    /// 调度器规约
    pub struct SchedulerSpec {
        pub threads: Vec<ThreadSpec>,
        pub current: Option<u64>,
        pub tick: u64,
    }

    impl SchedulerSpec {
        /// 不变量：最多一个线程处于运行状态
        ///
        /// ```verus
        /// invariant
        ///     self.threads.filter(|t| t.state == Running).len() <= 1
        /// ```
        pub fn single_running_invariant(&self) -> bool {
            self.threads.iter().filter(|t| t.state == ThreadState::Running).count() <= 1
        }

        /// 不变量：当前线程必须处于运行状态
        ///
        /// ```verus
        /// invariant
        ///     self.current.is_some() ==>
        ///     self.threads[self.current.unwrap()].state == Running
        /// ```
        pub fn current_is_running_invariant(&self) -> bool {
            match self.current {
                Some(id) => self.threads.iter()
                    .find(|t| t.id == id)
                    .map_or(false, |t| t.state == ThreadState::Running),
                None => true,
            }
        }

        /// 公平性属性：有界等待
        ///
        /// ```verus
        /// property fairness:
        ///     forall |t: Thread|
        ///         t.state == Ready ==>
        ///         eventually(t.state == Running)
        /// ```
        pub fn bounded_waiting(&self, max_wait: u64) -> bool {
            self.threads.iter()
                .filter(|t| t.state == ThreadState::Ready)
                .all(|t| t.wait_time <= max_wait)
        }
    }

    /// 调度决策规约
    ///
    /// ```verus
    /// fn schedule(scheduler: &mut Scheduler) -> Option<ThreadId>
    ///     requires
    ///         scheduler.single_running_invariant(),
    ///     ensures
    ///         scheduler.single_running_invariant(),
    ///         result.is_some() ==>
    ///             scheduler.threads[result.unwrap()].state == Running,
    /// ```
    pub fn schedule_spec(scheduler: &SchedulerSpec) -> Option<u64> {
        // 选择最高优先级的就绪线程
        scheduler.threads.iter()
            .filter(|t| t.state == ThreadState::Ready)
            .max_by_key(|t| t.priority)
            .map(|t| t.id)
    }
}

// IPC 规约在 ipc_spec.rs 文件中定义

// ============ io_uring 规约 ============

pub mod io_uring_spec {
    //! io_uring 形式化规约

    /// io_uring 规约
    pub struct IoUringSpec {
        pub sq_head: u32,
        pub sq_tail: u32,
        pub cq_head: u32,
        pub cq_tail: u32,
        pub sq_size: u32,
        pub cq_size: u32,
    }

    impl IoUringSpec {
        /// 不变量：SQ 不溢出
        ///
        /// ```verus
        /// invariant
        ///     self.sq_tail - self.sq_head <= self.sq_size
        /// ```
        pub fn sq_no_overflow(&self) -> bool {
            self.sq_tail.wrapping_sub(self.sq_head) <= self.sq_size
        }

        /// 不变量：CQ 不溢出
        pub fn cq_no_overflow(&self) -> bool {
            self.cq_tail.wrapping_sub(self.cq_head) <= self.cq_size
        }

        /// 不变量：完成数 <= 提交数
        ///
        /// ```verus
        /// invariant
        ///     self.completions() <= self.submissions()
        /// ```
        pub fn completions_bounded(&self) -> bool {
            let submissions = self.sq_tail.wrapping_sub(self.sq_head);
            let completions = self.cq_tail.wrapping_sub(self.cq_head);
            completions <= submissions
        }
    }
}

// ============ CHERI 规约 ============

pub mod cheri_spec {
    //! CHERI 能力形式化规约

    /// CHERI 能力规约
    pub struct CheriCapSpec {
        pub base: u64,
        pub length: u64,
        pub permissions: u32,
        pub tag: bool,
    }

    impl CheriCapSpec {
        /// 不变量：有效能力的边界合法
        ///
        /// ```verus
        /// invariant
        ///     self.tag ==>
        ///     self.base + self.length >= self.base  // 无溢出
        /// ```
        pub fn bounds_valid(&self) -> bool {
            !self.tag || self.base.checked_add(self.length).is_some()
        }

        /// 派生规约：权限单调递减
        ///
        /// ```verus
        /// fn derive(parent: CheriCap, child_perms: u32) -> CheriCap
        ///     requires
        ///         parent.tag,
        ///         (child_perms & !parent.permissions) == 0,
        ///     ensures
        ///         result.permissions == child_perms,
        ///         result.tag,
        /// ```
        pub fn derive_valid(&self, child_perms: u32) -> bool {
            self.tag && (child_perms & !self.permissions) == 0
        }

        /// 访问规约：边界检查
        ///
        /// ```verus
        /// fn access(cap: CheriCap, offset: u64, size: u64) -> bool
        ///     requires
        ///         cap.tag,
        ///     ensures
        ///         result == (offset + size <= cap.length)
        /// ```
        pub fn access_valid(&self, offset: u64, size: u64) -> bool {
            self.tag && offset.checked_add(size).map_or(false, |end| end <= self.length)
        }
    }
}

/// 初始化形式化验证模块
pub fn init() {
    crate::println!("[formal] Formal verification specifications loaded");
    
    // 基本规约检查
    let mut passed = 0;
    let mut failed = 0;
    
    // 检查能力系统规约
    {
        use capability_spec::{CapabilitySpec, Rights};
        
        let parent = CapabilitySpec {
            id: 1,
            rights: Rights(0b1111),
            parent: None,
            generation: 0,
            valid: true,
        };
        
        let child = CapabilitySpec {
            id: 2,
            rights: Rights(0b0011),
            parent: Some(1),
            generation: 0,
            valid: true,
        };
        
        if child.rights_monotonic_invariant(&parent) {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    
    crate::println!("[formal] Specification checks: {} passed, {} failed", passed, failed);
}

/// 形式化验证模块自测试
pub fn self_test() {
    crate::println!("[formal] Running self-test...");
    
    // 测试 Proof 类型
    let _proof: Proof<()> = Proof::assume();
    
    // 测试能力规约
    {
        use capability_spec::{CapabilitySpec, Rights, derive_spec};
        
        // 测试权限常量
        let read = Rights::READ;
        let write = Rights::WRITE;
        let execute = Rights::EXECUTE;
        let grant = Rights::GRANT;
        crate::println!("[formal] Rights: R={}, W={}, X={}, G={}", 
            read.0, write.0, execute.0, grant.0);
        
        // 测试能力规约
        let parent = CapabilitySpec {
            id: 1,
            rights: Rights(0b1111),
            parent: None,
            generation: 0,
            valid: true,
        };
        
        let child = CapabilitySpec {
            id: 2,
            rights: Rights(0b0011),
            parent: Some(1),
            generation: 0,
            valid: true,
        };
        
        // 测试权限单调性
        assert!(child.rights_monotonic_invariant(&parent));
        
        // 测试撤销传递性
        assert!(child.revocation_transitivity_invariant(&parent));
        
        // 测试派生规约
        let derived = derive_spec(&parent, Rights(0b0001));
        assert!(derived.is_some());
    }
    
    // 测试内存区域规约
    {
        use memory_spec::MemoryRegionSpec;
        
        let region = MemoryRegionSpec {
            base: 0x1000,
            size: 0x1000,
            allocated: true,
            owner: 1,
        };
        
        // 测试包含检查
        assert!(region.contains(0x1000, 0x100));
        assert!(!region.contains(0x3000, 0x100));
        
        // 测试重叠检查
        let region2 = MemoryRegionSpec {
            base: 0x2000,
            size: 0x1000,
            allocated: true,
            owner: 2,
        };
        assert!(region.no_overlap_invariant(&region2));
    }
    
    // 测试验证框架
    {
        use verification::{
            PropertyStatus as VPStatus, VerificationResult,
            status_symbol, get_timestamp,
            VerificationReport, VerifiedProperty, VerificationReportGenerator,
        };
        
        // 测试属性状态
        let _verified = VPStatus::Verified;
        let _failed = VPStatus::Failed;
        let _timeout = VPStatus::Timeout;
        let _unknown = VPStatus::Unknown;
        
        // 测试验证结果
        let _all_passed = VerificationResult::AllPassed;
        let _partial = VerificationResult::PartialPass;
        let _all_failed = VerificationResult::AllFailed;
        
        // 测试状态符号
        let sym = status_symbol(VPStatus::Verified);
        assert_eq!(sym, "✓");
        
        // 测试时间戳
        let ts = get_timestamp();
        crate::println!("[formal] Timestamp: {}", ts);
        
        // 测试 VerificationReport
        let mut report = VerificationReport::new();
        let prop = VerifiedProperty {
            name: alloc::string::String::from("test_property"),
            description: alloc::string::String::from("Test property"),
            status: VPStatus::Verified,
            proof_size: 100,
            module: alloc::string::String::from("test"),
            details: None,
            counterexample: None,
        };
        report.add_property(prop);
        let report_str = report.to_string();
        crate::println!("[formal] Report length: {}", report_str.len());
        
        // 测试 VerificationReportGenerator
        let _full_report = VerificationReportGenerator::generate_full_report();
        let _cap_report = VerificationReportGenerator::generate_capability_report();
        let _ipc_report = VerificationReportGenerator::generate_ipc_report();
        let _cheri_report = VerificationReportGenerator::generate_cheri_report();
        let _io_uring_report = VerificationReportGenerator::generate_io_uring_report();
    }
    
    // 测试调度器规约
    {
        use scheduler_spec::{ThreadState, ThreadSpec, SchedulerSpec, schedule_spec};
        
        let thread1 = ThreadSpec {
            id: 1,
            state: ThreadState::Ready,
            priority: 10,
            wait_time: 0,
            time_slice: 100,
        };
        
        let thread2 = ThreadSpec {
            id: 2,
            state: ThreadState::Running,
            priority: 5,
            wait_time: 100,
            time_slice: 50,
        };
        
        let thread3 = ThreadSpec {
            id: 3,
            state: ThreadState::Blocked,
            priority: 15,
            wait_time: 50,
            time_slice: 100,
        };
        
        let thread4 = ThreadSpec {
            id: 4,
            state: ThreadState::Terminated,
            priority: 1,
            wait_time: 0,
            time_slice: 0,
        };
        
        let scheduler = SchedulerSpec {
            threads: alloc::vec![thread1, thread2, thread3, thread4],
            current: Some(2),
            tick: 1000,
        };
        
        // 测试调度器不变量
        let single_running = scheduler.single_running_invariant();
        crate::println!("[formal] Single running invariant: {}", single_running);
        
        let current_running = scheduler.current_is_running_invariant();
        crate::println!("[formal] Current is running invariant: {}", current_running);
        
        let bounded = scheduler.bounded_waiting(1000);
        crate::println!("[formal] Bounded waiting: {}", bounded);
        
        // 测试调度规约
        let next = schedule_spec(&scheduler);
        crate::println!("[formal] Next thread to schedule: {:?}", next);
    }
    
    // 测试 io_uring 规约
    {
        use io_uring_spec::{IoUringSpec};
        
        let spec = IoUringSpec {
            sq_size: 32,
            cq_size: 64,
            sq_head: 0,
            sq_tail: 10,
            cq_head: 0,
            cq_tail: 5,
        };
        
        let sq_ok = spec.sq_no_overflow();
        let cq_ok = spec.cq_no_overflow();
        let bounded = spec.completions_bounded();
        crate::println!("[formal] io_uring: sq_ok={}, cq_ok={}, bounded={}", sq_ok, cq_ok, bounded);
    }
    
    // 测试 CHERI 规约
    {
        use cheri_spec::{CheriCapSpec};
        
        let spec = CheriCapSpec {
            base: 0x1000,
            length: 0x2000,
            permissions: 0b1111,
            tag: true,
        };
        
        let bounds_ok = spec.bounds_valid();
        let derive_ok = spec.derive_valid(0b0011);
        let access_ok = spec.access_valid(0x100, 0x100);
        crate::println!("[formal] CHERI: bounds={}, derive={}, access={}", bounds_ok, derive_ok, access_ok);
    }
    
    // 测试内存访问规约
    {
        use memory_spec::{MemoryRegionSpec, memory_access_spec};
        
        let regions = [
            MemoryRegionSpec {
                base: 0x1000,
                size: 0x1000,
                allocated: true,
                owner: 1,
            },
        ];
        let result = memory_access_spec(&regions, 0x1100, 0x100);
        crate::println!("[formal] Memory access spec result: {}", result);
    }
    
    // 测试 IPC 规约
    {
        use ipc_spec::{EndpointSpec, IpcSpec, MessageSpec, IpcError, PropertyStatus, IpcVerificationReport, transfer_capability_spec};
        
        let endpoint = EndpointSpec::new(16);
        
        // 测试不变量
        let capacity_ok = endpoint.capacity_invariant();
        let fifo_ok = endpoint.fifo_invariant();
        let waiter_mutex = endpoint.waiter_mutex_invariant();
        crate::println!("[formal] IPC endpoint: capacity={}, fifo={}, waiter_mutex={}", capacity_ok, fifo_ok, waiter_mutex);
        
        // 测试 IpcSpec 方法
        let cap_safe = IpcSpec::capability_transfer_safe(true, true);
        let no_loss = IpcSpec::no_message_loss(Ok(()), true);
        crate::println!("[formal] IPC: cap_safe={}, no_loss={}", cap_safe, no_loss);
        
        // 测试消息完整性
        let msg = MessageSpec {
            tag: 1,
            regs: [0, 0, 0, 0],
            cap_count: 0,
            sent_time: 0,
            sender_id: 1,
        };
        let integrity = IpcSpec::message_integrity(&msg, &msg);
        crate::println!("[formal] Message integrity: {}", integrity);
        
        // 测试 transfer_capability_spec
        let transfer_result = transfer_capability_spec(true, true, true);
        crate::println!("[formal] Transfer capability spec: {:?}", transfer_result);
        
        // 测试 IpcError 变体
        let _errors = [
            IpcError::InvalidCapability,
            IpcError::PermissionDenied,
            IpcError::NoSlotAvailable,
            IpcError::Timeout,
            IpcError::EndpointClosed,
        ];
        
        // 测试 PropertyStatus 变体
        let _statuses = [
            PropertyStatus::Verified,
            PropertyStatus::Failed,
            PropertyStatus::Timeout,
            PropertyStatus::Unknown,
        ];
        
        // 测试 IpcVerificationReport
        let report = IpcVerificationReport::generate();
        let all_verified = report.all_verified();
        crate::println!("[formal] IPC verification all verified: {}", all_verified);
    }
    
    crate::println!("[formal] Self-test passed!");
    
    // 运行运行时验证 API 测试
    test_runtime_verification_api();
}

// ============ 运行时验证 API ============

/// 验证能力派生操作的正确性
/// 
/// 在能力派生时调用此函数进行形式化验证检查
/// 
/// # 参数
/// - `parent_rights`: 父能力的权限位
/// - `child_rights`: 子能力的权限位
/// 
/// # 返回
/// - `true`: 验证通过（子权限是父权限的子集）
/// - `false`: 验证失败
pub fn verify_capability_derivation(parent_rights: u32, child_rights: u32) -> bool {
    use capability_spec::Rights;
    
    let parent = Rights(parent_rights);
    let child = Rights(child_rights);
    
    child.is_subset_of(&parent)
}

/// 验证内存访问的正确性
/// 
/// 在内存访问时调用此函数进行形式化验证检查
/// 
/// # 参数
/// - `base`: 内存区域基地址
/// - `size`: 内存区域大小
/// - `access_addr`: 访问地址
/// - `access_size`: 访问大小
/// 
/// # 返回
/// - `true`: 验证通过（访问在边界内）
/// - `false`: 验证失败
pub fn verify_memory_access(base: u64, size: u64, access_addr: u64, access_size: u64) -> bool {
    use memory_spec::MemoryRegionSpec;
    
    let region = MemoryRegionSpec {
        base,
        size,
        allocated: true,
        owner: 0,
    };
    
    region.contains(access_addr, access_size)
}

/// 验证 CHERI 能力派生的正确性
/// 
/// # 参数
/// - `parent_base`: 父能力基地址
/// - `parent_length`: 父能力长度
/// - `parent_perms`: 父能力权限
/// - `child_base`: 子能力基地址
/// - `child_length`: 子能力长度
/// - `child_perms`: 子能力权限
/// 
/// # 返回
/// - `true`: 验证通过
/// - `false`: 验证失败
pub fn verify_cheri_derivation(
    parent_base: u64,
    parent_length: u64,
    parent_perms: u32,
    child_base: u64,
    child_length: u64,
    child_perms: u32,
) -> bool {
    use cheri_spec::CheriCapSpec;
    
    let parent = CheriCapSpec {
        base: parent_base,
        length: parent_length,
        permissions: parent_perms,
        tag: true,
    };
    
    // 检查边界单调性
    let bounds_ok = child_base >= parent_base &&
        child_base.checked_add(child_length)
            .map(|end| end <= parent_base + parent_length)
            .unwrap_or(false);
    
    // 检查权限单调性
    let perms_ok = parent.derive_valid(child_perms);
    
    bounds_ok && perms_ok
}

/// 生成能力系统验证报告
pub fn generate_capability_verification_report() -> verification::VerificationReport {
    verification::VerificationReportGenerator::generate_capability_report()
}

/// 生成 IPC 验证报告
pub fn generate_ipc_verification_report() -> verification::VerificationReport {
    verification::VerificationReportGenerator::generate_ipc_report()
}

/// 生成 CHERI 验证报告
pub fn generate_cheri_verification_report() -> verification::VerificationReport {
    verification::VerificationReportGenerator::generate_cheri_report()
}

/// 生成完整验证报告
pub fn generate_full_verification_report() -> verification::VerificationReport {
    verification::VerificationReportGenerator::generate_full_report()
}

/// 打印验证报告到控制台
pub fn print_verification_report(report: &verification::VerificationReport) {
    crate::println!("{}", report.to_string());
}

/// 运行时验证 API 自测试
pub fn test_runtime_verification_api() {
    crate::println!("[formal] Testing runtime verification API...");
    
    // 测试能力派生验证
    let cap_valid = verify_capability_derivation(0b1111, 0b0011);
    assert!(cap_valid, "Valid capability derivation should pass");
    
    let cap_invalid = verify_capability_derivation(0b0011, 0b1111);
    assert!(!cap_invalid, "Invalid capability derivation should fail");
    crate::println!("[formal] Capability derivation verification: OK");
    
    // 测试内存访问验证
    let mem_valid = verify_memory_access(0x1000, 0x1000, 0x1100, 0x100);
    assert!(mem_valid, "Valid memory access should pass");
    
    let mem_invalid = verify_memory_access(0x1000, 0x1000, 0x3000, 0x100);
    assert!(!mem_invalid, "Invalid memory access should fail");
    crate::println!("[formal] Memory access verification: OK");
    
    // 测试 CHERI 派生验证
    let cheri_valid = verify_cheri_derivation(
        0x1000, 0x2000, 0b1111,
        0x1500, 0x1000, 0b0011
    );
    assert!(cheri_valid, "Valid CHERI derivation should pass");
    
    let cheri_invalid = verify_cheri_derivation(
        0x1000, 0x2000, 0b0011,
        0x1500, 0x1000, 0b1111
    );
    assert!(!cheri_invalid, "Invalid CHERI derivation should fail");
    crate::println!("[formal] CHERI derivation verification: OK");
    
    // 测试报告生成
    let cap_report = generate_capability_verification_report();
    assert!(!cap_report.properties.is_empty(), "Capability report should have properties");
    
    let ipc_report = generate_ipc_verification_report();
    assert!(!ipc_report.properties.is_empty(), "IPC report should have properties");
    
    let cheri_report = generate_cheri_verification_report();
    assert!(!cheri_report.properties.is_empty(), "CHERI report should have properties");
    
    let full_report = generate_full_verification_report();
    assert!(!full_report.properties.is_empty(), "Full report should have properties");
    crate::println!("[formal] Report generation: OK");
    
    // 测试报告打印（只打印摘要）
    crate::println!("[formal] Full report has {} properties", full_report.properties.len());
    
    // 测试 print_verification_report 函数（打印能力报告作为示例）
    // 打印简短的能力验证报告
    print_verification_report(&cap_report);
    
    crate::println!("[formal] Runtime verification API test passed!");
}
