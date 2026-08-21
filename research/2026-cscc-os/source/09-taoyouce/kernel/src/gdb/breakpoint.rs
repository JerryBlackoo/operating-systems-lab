//! 断点管理
//!
//! 实现软件断点和硬件断点支持

/// 断点类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakpointType {
    /// 软件断点（替换指令为 ebreak）
    Software,
    /// 硬件断点（使用调试寄存器）
    Hardware,
    /// 写监视点
    WriteWatchpoint,
    /// 读监视点
    ReadWatchpoint,
    /// 访问监视点
    AccessWatchpoint,
    /// 单步执行临时断点
    SingleStep,
}

/// 断点
#[derive(Debug, Clone)]
pub struct Breakpoint {
    /// 断点地址
    pub addr: u64,
    /// 原始指令（用于恢复）
    pub original_insn: u32,
    /// 断点类型
    pub bp_type: BreakpointType,
    /// 是否启用
    pub enabled: bool,
}

impl Breakpoint {
    /// 创建软件断点
    pub fn software(addr: u64) -> Self {
        Self {
            addr,
            original_insn: 0,
            bp_type: BreakpointType::Software,
            enabled: false,
        }
    }

    /// 创建硬件断点
    pub fn hardware(addr: u64) -> Self {
        Self {
            addr,
            original_insn: 0,
            bp_type: BreakpointType::Hardware,
            enabled: false,
        }
    }

    /// 创建写监视点
    pub fn write_watchpoint(addr: u64) -> Self {
        Self {
            addr,
            original_insn: 0,
            bp_type: BreakpointType::WriteWatchpoint,
            enabled: false,
        }
    }

    /// 创建读监视点
    pub fn read_watchpoint(addr: u64) -> Self {
        Self {
            addr,
            original_insn: 0,
            bp_type: BreakpointType::ReadWatchpoint,
            enabled: false,
        }
    }

    /// 创建访问监视点
    pub fn access_watchpoint(addr: u64) -> Self {
        Self {
            addr,
            original_insn: 0,
            bp_type: BreakpointType::AccessWatchpoint,
            enabled: false,
        }
    }

    /// 启用断点
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// 禁用断点
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// 检查是否是软件断点
    pub fn is_software(&self) -> bool {
        self.bp_type == BreakpointType::Software
    }

    /// 检查是否是监视点
    pub fn is_watchpoint(&self) -> bool {
        matches!(
            self.bp_type,
            BreakpointType::WriteWatchpoint
                | BreakpointType::ReadWatchpoint
                | BreakpointType::AccessWatchpoint
        )
    }
}

/// 断点管理器
pub struct BreakpointManager {
    /// 软件断点列表
    software_breakpoints: [Option<Breakpoint>; 32],
    /// 硬件断点列表（RISC-V 通常支持 2-4 个）
    hardware_breakpoints: [Option<Breakpoint>; 4],
    /// 监视点列表
    watchpoints: [Option<Breakpoint>; 4],
}

impl BreakpointManager {
    /// 创建新的断点管理器
    pub const fn new() -> Self {
        const NONE_BP: Option<Breakpoint> = None;
        Self {
            software_breakpoints: [NONE_BP; 32],
            hardware_breakpoints: [NONE_BP; 4],
            watchpoints: [NONE_BP; 4],
        }
    }

    /// 添加软件断点
    pub fn add_software_breakpoint(&mut self, addr: u64) -> Option<usize> {
        for (i, slot) in self.software_breakpoints.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Breakpoint::software(addr));
                return Some(i);
            }
        }
        None
    }

    /// 添加硬件断点
    pub fn add_hardware_breakpoint(&mut self, addr: u64) -> Option<usize> {
        for (i, slot) in self.hardware_breakpoints.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Breakpoint::hardware(addr));
                return Some(i);
            }
        }
        None
    }

    /// 添加监视点
    pub fn add_watchpoint(&mut self, addr: u64, bp_type: BreakpointType) -> Option<usize> {
        for (i, slot) in self.watchpoints.iter_mut().enumerate() {
            if slot.is_none() {
                let mut bp = Breakpoint::software(addr);
                bp.bp_type = bp_type;
                *slot = Some(bp);
                return Some(i);
            }
        }
        None
    }

    /// 移除断点
    pub fn remove_breakpoint(&mut self, addr: u64) -> Option<Breakpoint> {
        // 检查软件断点
        for slot in &mut self.software_breakpoints {
            if let Some(bp) = slot {
                if bp.addr == addr {
                    return slot.take();
                }
            }
        }

        // 检查硬件断点
        for slot in &mut self.hardware_breakpoints {
            if let Some(bp) = slot {
                if bp.addr == addr {
                    return slot.take();
                }
            }
        }

        // 检查监视点
        for slot in &mut self.watchpoints {
            if let Some(bp) = slot {
                if bp.addr == addr {
                    return slot.take();
                }
            }
        }

        None
    }

    /// 查找断点
    pub fn find_breakpoint(&self, addr: u64) -> Option<&Breakpoint> {
        for slot in &self.software_breakpoints {
            if let Some(bp) = slot {
                if bp.addr == addr {
                    return Some(bp);
                }
            }
        }

        for slot in &self.hardware_breakpoints {
            if let Some(bp) = slot {
                if bp.addr == addr {
                    return Some(bp);
                }
            }
        }

        None
    }

    /// 获取所有启用的断点
    pub fn enabled_breakpoints(&self) -> impl Iterator<Item = &Breakpoint> {
        self.software_breakpoints
            .iter()
            .chain(self.hardware_breakpoints.iter())
            .chain(self.watchpoints.iter())
            .filter_map(|slot| slot.as_ref())
            .filter(|bp| bp.enabled)
    }

    /// 清除所有断点
    pub fn clear_all(&mut self) {
        for slot in &mut self.software_breakpoints {
            *slot = None;
        }
        for slot in &mut self.hardware_breakpoints {
            *slot = None;
        }
        for slot in &mut self.watchpoints {
            *slot = None;
        }
    }
}

impl Default for BreakpointManager {
    fn default() -> Self {
        Self::new()
    }
}
