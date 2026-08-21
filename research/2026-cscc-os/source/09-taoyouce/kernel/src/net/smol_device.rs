//! smoltcp Device trait 适配层 — 桥接 VirtIO-net 与 smoltcp 网络协议栈
//!
//! 🟡 微内核适配：实现 smoltcp 的 `phy::Device` trait，将 VirtIO-net 的
//! 收发接口（基于 virtio-drivers crate）桥接到 smoltcp 的 `RxToken`/`TxToken` 模型。
//!
//! 来源: 适配层代码为 RustMicroOS 原创
//! 复用: smoltcp crate (BSD-0-Clause License, https://github.com/smoltcp-rs/smoltcp)
//!       virtio-drivers crate (MIT License, https://github.com/rcore-os/virtio-drivers)
//!
//! # 设计说明
//!
//! smoltcp 通过 `phy::Device` trait 抽象底层网络硬件。适配层需要提供：
//!
//! - `receive()`: 从 VirtIO-net 接收数据包，返回 `RxToken` + `TxToken`
//! - `transmit()`: 获取发送令牌 `TxToken`，用于发送数据包
//! - `capabilities()`: 报告设备能力（MTU、介质类型等）
//!
//! # 参考
//!
//! - ArceOS 项目的 `axnet` 模块: smoltcp Device trait 适配
//! - Starry 项目的 smoltcp 集成: VirtIO-net 桥接方案

use alloc::vec::Vec;
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// VirtIO-net 最大传输单元（以太网帧最大长度）
/// Maximum Transmission Unit for VirtIO-net (Ethernet frame max size)
const VIRTIO_NET_MTU: usize = 1514;

// ============================================================================
// SmolVirtioNet — smoltcp Device trait 适配器
// ============================================================================

/// smoltcp 网络设备适配器 — 桥接 VirtIO-net 与 smoltcp 协议栈
///
/// 🟡 微内核适配：通过 `crate::driver::virtio::net` 模块的全局函数
/// （`recv_packet`/`send_packet`）访问 VirtIO-net 设备，无需持有设备引用。
///
/// 这种设计避免了在 smoltcp 的 `Device` trait 中传递复杂的锁和引用，
/// 与微内核的全局设备管理模式一致。
///
/// # 使用方式
///
/// ```rust,ignore
/// let mut device = SmolVirtioNet::new();
/// let mut iface = Interface::new(config, &mut device, Instant::ZERO);
/// // 在网络轮询中使用:
/// iface.poll(timestamp, &mut device, &mut sockets);
/// ```
pub struct SmolVirtioNet {
    /// 接收缓冲区 — 缓存从 VirtIO-net 接收到的数据包
    /// Receive buffer — caches packets received from VirtIO-net
    ///
    /// smoltcp 的 `receive()` 方法需要同时返回 RxToken 和 TxToken，
    /// 但 VirtIO-net 的接收是即时的。我们在 `receive()` 中先尝试
    /// 从 VirtIO-net 读取一个包，如果成功则缓存在此处。
    rx_buffer: Option<Vec<u8>>,
}

impl SmolVirtioNet {
    /// 创建新的 smoltcp 网络设备适配器
    /// Create a new smoltcp network device adapter
    pub fn new() -> Self {
        Self { rx_buffer: None }
    }
}

// ============================================================================
// smoltcp phy::Device trait 实现
// ============================================================================

impl Device for SmolVirtioNet {
    type RxToken<'a> = VirtioRxToken where Self: 'a;
    type TxToken<'a> = VirtioTxToken where Self: 'a;

    /// 尝试接收一个数据包
    ///
    /// 从 VirtIO-net 设备接收数据包。如果有数据包可用，返回
    /// `(RxToken, TxToken)` 元组；否则返回 `None`。
    ///
    /// smoltcp 要求 `receive()` 同时返回 RxToken 和 TxToken，
    /// 这是因为协议栈在处理接收到的包时可能需要立即发送响应
    /// （例如 TCP ACK、ARP 回复等）。
    ///
    /// # 实现细节
    ///
    /// 调用 `crate::driver::virtio::net::recv_packet()` 从 VirtIO-net
    /// 接收数据包。该函数是非阻塞的，如果没有数据包则返回 None。
    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // 尝试从 VirtIO-net 接收数据包
        // Try to receive a packet from VirtIO-net
        match crate::driver::virtio::net::recv_packet() {
            Some(buf) => {
                // 成功接收到数据包，返回 RxToken 和 TxToken
                // Successfully received a packet, return RxToken and TxToken
                Some((
                    VirtioRxToken { buffer: buf },
                    VirtioTxToken,
                ))
            }
            None => None,
        }
    }

    /// 获取发送令牌
    ///
    /// 返回一个 `TxToken`，调用者可以通过它发送数据包。
    /// VirtIO-net 设备始终可以接受发送请求（队列满时由底层驱动处理），
    /// 因此总是返回 `Some`。
    ///
    /// # 注意
    ///
    /// 如果 VirtIO-net 设备未初始化，发送操作会在 `TxToken::consume()`
    /// 中失败并静默丢弃数据包。
    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtioTxToken)
    }

    /// 报告设备能力
    ///
    /// 返回 VirtIO-net 设备的能力信息，包括：
    /// - MTU: 1514 字节（标准以太网帧最大长度）
    /// - 介质类型: Ethernet
    /// - 校验和能力: 无硬件校验和卸载（由 smoltcp 软件计算）
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        // 标准以太网 MTU = 1514 字节（14 字节以太网头 + 1500 字节载荷）
        // Standard Ethernet MTU = 1514 bytes (14-byte Ethernet header + 1500-byte payload)
        caps.max_transmission_unit = VIRTIO_NET_MTU;
        // 以太网介质类型
        // Ethernet medium type
        caps.medium = Medium::Ethernet;
        // 不使用硬件校验和卸载，由 smoltcp 软件计算
        // No hardware checksum offload, computed by smoltcp in software
        caps.checksum = smoltcp::phy::ChecksumCapabilities::default();
        caps
    }
}

// ============================================================================
// VirtioRxToken — 接收令牌
// ============================================================================

/// VirtIO-net 接收令牌 — 封装从 VirtIO-net 接收到的数据包
///
/// smoltcp 通过 `RxToken::consume()` 回调处理接收到的数据包。
/// 令牌持有数据包的所有权，在 `consume()` 中将数据传递给 smoltcp 协议栈。
pub struct VirtioRxToken {
    /// 接收到的原始以太网帧数据
    /// Raw Ethernet frame data received from VirtIO-net
    buffer: Vec<u8>,
}

impl phy::RxToken for VirtioRxToken {
    /// 消费接收令牌，将数据包传递给 smoltcp 协议栈处理
    ///
    /// `f` 是 smoltcp 提供的回调函数，它会解析以太网帧并进行协议处理
    /// （ARP、IP、TCP/UDP 等）。
    ///
    /// # 参数
    ///
    /// - `f`: 处理函数，接收数据包切片 `&mut [u8]`，返回处理结果 `R`
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // 将接收到的数据包传递给 smoltcp 处理
        // Pass the received packet to smoltcp for processing
        f(&mut self.buffer)
    }
}

// ============================================================================
// VirtioTxToken — 发送令牌
// ============================================================================

/// VirtIO-net 发送令牌 — 用于向 VirtIO-net 发送数据包
///
/// smoltcp 通过 `TxToken::consume()` 回调构造并发送数据包。
/// 令牌在 `consume()` 中分配发送缓冲区，让 smoltcp 填充数据，
/// 然后通过 VirtIO-net 发送。
pub struct VirtioTxToken;

impl phy::TxToken for VirtioTxToken {
    /// 消费发送令牌，构造并发送数据包
    ///
    /// `f` 是 smoltcp 提供的回调函数，它会在提供的缓冲区中构造
    /// 完整的以太网帧（包括以太网头、IP 头、TCP/UDP 头和载荷）。
    ///
    /// # 参数
    ///
    /// - `len`: smoltcp 请求的发送缓冲区大小
    /// - `f`: 构造函数，接收缓冲区切片 `&mut [u8]`，填充数据包内容
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // 分配发送缓冲区
        // Allocate transmit buffer
        let mut buffer = alloc::vec![0u8; len];

        // 让 smoltcp 填充数据包内容
        // Let smoltcp fill in the packet content
        let result = f(&mut buffer);

        // 通过 VirtIO-net 发送数据包
        // Send the packet via VirtIO-net
        if let Err(e) = crate::driver::virtio::net::send_packet(buffer) {
            // 发送失败时记录警告日志，但不影响 smoltcp 的处理流程
            // Log warning on send failure, but don't affect smoltcp's processing flow
            log::warn!("[smol-device] Failed to send packet (len={}): {}", len, e);
        }

        result
    }
}
