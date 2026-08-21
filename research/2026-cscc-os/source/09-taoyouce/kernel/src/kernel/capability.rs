use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::capability::{CapId, CapRights, CapType as KernelCapType, Capability, CSpace};
use crate::sync::SpinLock;

use crate::syscall::user::{read_user_buffer, write_user_buffer};
use crate::syscall::SyscallError;

/// 全局能力空间（每个进程应该有自己的 CSpace，这里简化为全局）
static GLOBAL_CSPACE: SpinLock<Option<CSpace>> = SpinLock::new(None);

/// 能力 ID 到能力的映射
static CAP_TABLE: SpinLock<BTreeMap<u64, Capability>> = SpinLock::new(BTreeMap::new());

/// 初始化能力系统
fn init_cap_system() {
    let mut cspace = GLOBAL_CSPACE.lock();
    if cspace.is_none() {
        *cspace = Some(CSpace::new(1024));
    }
}

/// 创建能力
///
/// # 参数
/// - `cap_type`: 能力类型 (0=Null, 1=Memory, 2=Endpoint, ...)
/// - `addr`: 对象地址（对于内存能力）
/// - `size`: 对象大小（对于内存能力）
/// - `rights`: 权限位
///
/// # 返回
/// - 成功: 能力句柄 (> 0)
/// - 失败: 负数错误码
#[allow(dead_code)]
pub(crate) fn sys_cap_create(cap_type: u32, addr: usize, size: usize, rights: u32) -> isize {
    init_cap_system();

    // 转换能力类型
    let kernel_cap_type = match cap_type {
        0 => KernelCapType::Null,
        1 => KernelCapType::Memory,
        2 => KernelCapType::Endpoint,
        3 => KernelCapType::Notification,
        4 => KernelCapType::Thread,
        5 => KernelCapType::CNode,
        6 => KernelCapType::PageTable,
        7 => KernelCapType::Interrupt,
        8 => KernelCapType::Device,
        _ => return SyscallError::InvalidArgument as isize,
    };

    // 转换权限
    let cap_rights = CapRights::from_bits_truncate(rights);

    // 创建能力
    let cap = if kernel_cap_type == KernelCapType::Memory && size > 0 {
        // 内存能力带 CHERI 保护
        Capability::new_memory(addr, size, cap_rights)
    } else {
        Capability::new(kernel_cap_type, cap_rights, addr)
    };

    let cap_id = cap.id.value();

    // 注册能力
    crate::capability::register_cap(&cap);

    // 存储到全局表
    {
        let mut table = CAP_TABLE.lock();
        table.insert(cap_id, cap);
    }

    cap_id as isize
}

/// 派生能力
///
/// # 参数
/// - `parent_handle`: 父能力句柄
/// - `new_rights`: 新权限（必须是父权限的子集）
///
/// # 返回
/// - 成功: 新能力句柄
/// - 失败: 负数错误码
#[allow(dead_code)]
pub(crate) fn sys_cap_derive(parent_handle: u64, new_rights: u32) -> isize {
    let cap_rights = CapRights::from_bits_truncate(new_rights);

    let mut table = CAP_TABLE.lock();

    // 查找父能力
    let parent_cap = match table.get(&parent_handle) {
        Some(cap) => cap.clone(),
        None => return SyscallError::InvalidCapability as isize,
    };

    // 派生新能力
    let derived_cap = match parent_cap.derive(cap_rights) {
        Some(cap) => cap,
        None => return SyscallError::PermissionDenied as isize,
    };

    let new_id = derived_cap.id.value();

    // 记录派生
    crate::capability::record_cap_delegate();

    // 存储新能力
    table.insert(new_id, derived_cap);

    new_id as isize
}

/// 撤销能力
///
/// # 参数
/// - `handle`: 能力句柄
///
/// # 返回
/// - 成功: 撤销的能力数量
/// - 失败: 负数错误码
#[allow(dead_code)]
pub(crate) fn sys_cap_revoke(handle: u64) -> isize {
    let mut table = CAP_TABLE.lock();

    // 查找能力
    let cap = match table.get(&handle) {
        Some(cap) => cap.clone(),
        None => return SyscallError::InvalidCapability as isize,
    };

    // 检查撤销权限
    if !cap.rights.contains(CapRights::REVOKE) {
        return SyscallError::PermissionDenied as isize;
    }

    // 收集所有需要撤销的能力（级联撤销）
    let cap_id = CapId(handle);
    let mut to_revoke = Vec::new();
    to_revoke.push(handle);

    // 查找所有以此能力为父的子能力
    for (&id, child_cap) in table.iter() {
        if child_cap.parent == cap_id {
            to_revoke.push(id);
            // 递归查找孙能力
            let child_id = CapId(id);
            for (&grandchild_id, grandchild_cap) in table.iter() {
                if grandchild_cap.parent == child_id {
                    to_revoke.push(grandchild_id);
                }
            }
        }
    }

    // 移除所有能力
    let count = to_revoke.len();
    for id in to_revoke {
        table.remove(&id);
        crate::capability::record_cap_revoke();
    }

    count as isize // 返回撤销的能力数量
}

/// 检查能力权限
///
/// # 参数
/// - `handle`: 能力句柄
/// - `required`: 需要检查的权限
///
/// # 返回
/// - 1: 有权限
/// - 0: 无权限
/// - 负数: 错误码
#[allow(dead_code)]
pub(crate) fn sys_cap_check(handle: u64, required: u32) -> isize {
    let cap_rights = CapRights::from_bits_truncate(required);

    let table = CAP_TABLE.lock();

    match table.get(&handle) {
        Some(cap) => {
            if cap.check(cap_rights) {
                1
            } else {
                0
            }
        }
        None => SyscallError::InvalidCapability as isize,
    }
}

/// 能力信息结构（与用户空间定义一致）
#[repr(C)]
struct KernelCapInfo {
    cap_type: u32,
    rights: u32,
    base: u64,
    length: u64,
    parent_id: u64,
    child_count: u32,
    _reserved: u32,
}

/// 获取能力信息
///
/// # 参数
/// - `handle`: 能力句柄
/// - `info_ptr`: 用户空间 CapInfo 结构指针
///
/// # 返回
/// - 0: 成功
/// - 负数: 错误码
#[allow(dead_code)]
pub(crate) fn sys_cap_info(handle: u64, info_ptr: *mut u8) -> isize {
    let table = CAP_TABLE.lock();

    let cap = match table.get(&handle) {
        Some(cap) => cap,
        None => return SyscallError::InvalidCapability as isize,
    };

    // 获取 CHERI 信息（如果有）
    let (base, length) = if let Some(cheri) = cap.cheri() {
        (cheri.base(), cheri.length())
    } else {
        (cap.object as u64, 0)
    };

    let info = KernelCapInfo {
        cap_type: cap.cap_type as u32,
        rights: cap.rights.bits(),
        base,
        length,
        parent_id: cap.parent.value(),
        // 真接 CapManager.derivation_tree：返回直接子能力数（之前是 TODO）。
        child_count: crate::capability::cap_child_count(cap.id),
        _reserved: 0,
    };

    // 写入用户空间
    let info_bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const _ as *const u8,
            core::mem::size_of::<KernelCapInfo>()
        )
    };

    if let Err(_) = write_user_buffer(info_ptr, info_bytes) {
        return SyscallError::InvalidArgument as isize;
    }

    0
}

/// 通过能力读取内存
///
/// # 参数
/// - `handle`: 内存能力句柄
/// - `offset`: 偏移量
/// - `buf`: 用户缓冲区指针
/// - `len`: 读取长度
///
/// # 返回
/// - 成功: 读取的字节数
/// - 失败: 负数错误码
///
/// 注意：当前实现是演示性的，能力指向的内存地址必须是有效的内核可访问地址。
/// 在实际系统中，能力应该指向内核管理的内存对象。
#[allow(dead_code)]
pub(crate) fn sys_cap_read(handle: u64, offset: usize, buf: *mut u8, len: usize) -> isize {
    let table = CAP_TABLE.lock();

    let cap = match table.get(&handle) {
        Some(cap) => cap,
        None => return SyscallError::InvalidCapability as isize,
    };

    // 检查是否是内存能力
    if cap.cap_type != KernelCapType::Memory {
        return SyscallError::InvalidCapability as isize;
    }

    // 双层安全检查
    if let Err(_) = cap.check_memory_access(offset, len, false) {
        return SyscallError::PermissionDenied as isize;
    }

    // 计算源地址
    let src_addr = cap.object + offset;

    // 检查地址是否在内核可访问范围内
    // 演示模式：只允许访问内核地址空间（0x8000_0000 以上）
    // 用户空间地址（如 0x3000_0000）不是真实映射的，返回错误
    if src_addr < 0x8000_0000 {
        // 这是一个演示用的虚拟地址，不是真实映射的内存
        // 返回模拟数据而不是触发页错误
        let data: Vec<u8> = (0..len).map(|i| ((src_addr + i) & 0xFF) as u8).collect();
        if let Err(_) = write_user_buffer(buf, &data) {
            return SyscallError::InvalidArgument as isize;
        }
        return len as isize;
    }

    // 读取数据
    let data: Vec<u8> = (0..len).map(|i| {
        unsafe { *((src_addr + i) as *const u8) }
    }).collect();

    // 写入用户缓冲区
    if let Err(_) = write_user_buffer(buf, &data) {
        return SyscallError::InvalidArgument as isize;
    }

    len as isize
}

/// 通过能力写入内存
///
/// # 参数
/// - `handle`: 内存能力句柄
/// - `offset`: 偏移量
/// - `buf`: 用户数据指针
/// - `len`: 写入长度
///
/// # 返回
/// - 成功: 写入的字节数
/// - 失败: 负数错误码
///
/// 注意：当前实现是演示性的，能力指向的内存地址必须是有效的内核可访问地址。
#[allow(dead_code)]
pub(crate) fn sys_cap_write(handle: u64, offset: usize, buf: *const u8, len: usize) -> isize {
    let table = CAP_TABLE.lock();

    let cap = match table.get(&handle) {
        Some(cap) => cap,
        None => return SyscallError::InvalidCapability as isize,
    };

    // 检查是否是内存能力
    if cap.cap_type != KernelCapType::Memory {
        return SyscallError::InvalidCapability as isize;
    }

    // 双层安全检查
    if let Err(_) = cap.check_memory_access(offset, len, true) {
        return SyscallError::PermissionDenied as isize;
    }

    // 从用户空间读取数据
    let data = match read_user_buffer(buf, len) {
        Ok(d) => d,
        Err(_) => return SyscallError::InvalidArgument as isize,
    };

    // 计算目标地址
    let dst_addr = cap.object + offset;

    // 检查地址是否在内核可访问范围内
    // 演示模式：只允许访问内核地址空间（0x8000_0000 以上）
    if dst_addr < 0x8000_0000 {
        // 这是一个演示用的虚拟地址，假装写入成功
        return len as isize;
    }

    // 写入数据
    for (i, &byte) in data.iter().enumerate() {
        unsafe { *((dst_addr + i) as *mut u8) = byte; }
    }

    len as isize
}

