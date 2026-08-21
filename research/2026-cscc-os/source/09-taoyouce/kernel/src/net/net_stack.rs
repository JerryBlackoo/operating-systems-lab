//! 全局网络栈单例 — 封装 smoltcp Interface + SocketSet
//!
//! 🟡 微内核适配：创建全局 `NetStack` 单例，配置 QEMU user-mode networking
//! 默认参数（IP=10.0.2.15/24, 网关=10.0.2.2, DNS=10.0.2.3），
//! 并实现事件驱动的网络轮询机制。
//!
//! 来源: 适配层代码为 RustMicroOS 原创
//! 复用: smoltcp crate (BSD-0-Clause License, https://github.com/smoltcp-rs/smoltcp)
//!       virtio-drivers crate (MIT License, https://github.com/rcore-os/virtio-drivers)
//!
//! # 设计说明
//!
//! NetStack 是网络子系统的核心单例，封装了：
//! - `smoltcp::iface::Interface`: 网络接口（IP 配置、路由表、ARP 缓存）
//! - `smoltcp::iface::SocketSet`: Socket 集合（TCP/UDP/DNS/ICMP socket 池）
//! - `SmolVirtioNet`: VirtIO-net 设备适配器（Device trait 实现）
//!
//! # 网络轮询机制
//!
//! 采用事件驱动 + 定时轮询混合方案：
//! - 中断驱动：VirtIO-net 中断触发时调用 `poll_net()`
//! - 定时轮询：定时器中断中周期性调用 `poll_net()` 作为兜底
//! - 主动轮询：socket 系统调用（send/recv）前后调用 `poll_net()`
//!
//! # 参考
//!
//! - ArceOS 项目的 `axnet/src/smoltcp_impl/`: 网络栈初始化和轮询
//! - Starry 项目的 smoltcp 集成: 全局 NetStack 管理

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;
use crate::sync::SpinLock;
use super::smol_device::SmolVirtioNet;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address};

// ============================================================================
// QEMU user-mode networking 默认参数
// QEMU user-mode networking default parameters
// ============================================================================

/// 默认 IP 地址 — QEMU user-mode networking 分配给 guest 的地址
/// Default IP address — assigned to guest by QEMU user-mode networking
const DEFAULT_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);

/// 判断 IP 是否为本机 iface 地址（用于本地投递路由判定）
pub(in crate::net) fn is_local_ip(ip: [u8; 4]) -> bool {
    Ipv4Address::new(ip[0], ip[1], ip[2], ip[3]) == DEFAULT_IP
}

/// 默认子网掩码前缀长度 — /24 即 255.255.255.0
/// Default subnet prefix length — /24 means 255.255.255.0
const DEFAULT_PREFIX_LEN: u8 = 24;

/// 默认网关 — QEMU user-mode networking 的网关地址
/// Default gateway — QEMU user-mode networking gateway address
const DEFAULT_GATEWAY: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);

/// 默认 DNS 服务器 — QEMU user-mode networking 的 DNS 地址
/// Default DNS server — QEMU user-mode networking DNS address
const DEFAULT_DNS: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

/// TCP socket 接收缓冲区大小（64KB）
/// TCP socket receive buffer size (64KB)
const TCP_RX_BUF_SIZE: usize = 65536;

/// TCP socket 发送缓冲区大小（64KB）
/// TCP socket transmit buffer size (64KB)
const TCP_TX_BUF_SIZE: usize = 65536;

/// UDP socket 接收缓冲区元数据槽数
/// UDP socket receive buffer metadata slot count
const UDP_RX_META_COUNT: usize = 32;

/// UDP socket 发送缓冲区元数据槽数
/// UDP socket transmit buffer metadata slot count
const UDP_TX_META_COUNT: usize = 32;

/// UDP socket 接收缓冲区大小（64KB）
/// UDP socket receive buffer size (64KB)
const UDP_RX_BUF_SIZE: usize = 65536;

/// UDP socket 发送缓冲区大小（64KB）
/// UDP socket transmit buffer size (64KB)
const UDP_TX_BUF_SIZE: usize = 65536;

// ============================================================================
// NetStack 全局单例 / NetStack Global Singleton
// ============================================================================

/// 全局网络栈 — 封装 smoltcp Interface + SocketSet + Device
///
/// 🟡 微内核适配：作为内核态网络栈的核心数据结构，
/// 通过 SpinLock 保护并发访问。所有网络操作（socket 系统调用、
/// 中断处理、定时轮询）都通过此单例进行。
///
/// # 线程安全
///
/// 使用 `SpinLock` 保护，适用于中断上下文和进程上下文。
/// 注意：持有锁期间不应进行长时间操作。
pub struct NetStack {
    /// smoltcp 网络接口 — 管理 IP 配置、路由表、ARP 缓存
    /// smoltcp network interface — manages IP config, routing table, ARP cache
    iface: Interface,

    /// smoltcp 设备适配器 — 桥接 VirtIO-net 收发
    /// smoltcp device adapter — bridges VirtIO-net send/receive
    device: SmolVirtioNet,

    /// smoltcp Socket 集合 — 管理所有活跃的 TCP/UDP/DNS socket
    /// smoltcp socket set — manages all active TCP/UDP/DNS sockets
    sockets: SocketSet<'static>,
}

/// 全局网络栈实例
/// Global network stack instance
///
/// 使用 `Option` 包装，在 `init()` 中初始化。
/// 使用 `SpinLock` 保护并发访问。
static NET_STACK: SpinLock<Option<NetStack>> = SpinLock::new(None);

/// 网络栈是否已初始化的标志。
///
/// 必须是无锁原子：`is_initialized()` 由 `on_timer_tick()` 在**每个 hart 的定时器
/// 中断**里调用，用自旋锁包一个 bool 等于把一把全局锁塞进中断路径。多核实测
/// （GDB 全 vCPU 取证 `rv-smpgdb-20260811T022657Z.gdb.txt`）：7 个 hart 同时停在
/// `on_timer_tick -> is_initialized -> SpinLock::lock` 的 CAS 自旋上，整机停摆。
/// 对标 Linux：这类"子系统是否就绪"的标志用 `static bool`/原子读，不加锁。
static NET_INITIALIZED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// ============================================================================
// 初始化 / Initialization
// ============================================================================

/// 初始化全局网络栈
///
/// 🟡 微内核适配：从 VirtIO-net 设备读取 MAC 地址，配置 smoltcp Interface
/// 使用 QEMU user-mode networking 默认参数。
///
/// # 调用时机
///
/// 应在 VirtIO 设备初始化完成后调用（`driver::virtio::init()` 之后）。
///
/// # 错误处理
///
/// 如果 VirtIO-net 设备未初始化（无 MAC 地址），则使用默认 MAC 地址
/// 并输出警告日志。网络功能可能不可用，但不会导致内核 panic。
///
/// Initialize the global network stack.
/// Should be called after VirtIO device initialization.
pub fn init() {
    // 防止重复初始化（原子 CAS，无锁；见 NET_INITIALIZED 的文档）。
    if NET_INITIALIZED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        log::warn!("[net-stack] Already initialized, skipping");
        return;
    }

    // 从 VirtIO-net 设备读取 MAC 地址
    // Read MAC address from VirtIO-net device
    let mac_bytes = crate::driver::virtio::net::get_mac().unwrap_or_else(|| {
        log::warn!("[net-stack] VirtIO-net not available, using default MAC");
        // 默认 MAC 地址（QEMU 默认值）
        // Default MAC address (QEMU default)
        [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]
    });

    let mac_addr = EthernetAddress(mac_bytes);

    crate::println!(
        "[net-stack] Initializing with MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac_bytes[0], mac_bytes[1], mac_bytes[2],
        mac_bytes[3], mac_bytes[4], mac_bytes[5]
    );

    // 创建 smoltcp 设备适配器
    // Create smoltcp device adapter
    let mut device = SmolVirtioNet::new();

    // 配置 smoltcp Interface
    // Configure smoltcp Interface
    let config = Config::new(mac_addr.into());
    let mut iface = Interface::new(config, &mut device, current_timestamp());

    // 配置 IP 地址 — 10.0.2.15/24
    // Configure IP address — 10.0.2.15/24
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs.push(IpCidr::new(IpAddress::Ipv4(DEFAULT_IP), DEFAULT_PREFIX_LEN))
            .expect("[net-stack] Failed to add IP address");
    });

    // 配置默认网关 — 10.0.2.2
    // Configure default gateway — 10.0.2.2
    iface.routes_mut()
        .add_default_ipv4_route(DEFAULT_GATEWAY)
        .expect("[net-stack] Failed to add default route");

    crate::println!(
        "[net-stack] IP: {}/{}, Gateway: {}, DNS: {}",
        DEFAULT_IP, DEFAULT_PREFIX_LEN, DEFAULT_GATEWAY, DEFAULT_DNS
    );

    // 创建 Socket 集合
    // Create socket set
    let sockets = SocketSet::new(vec![]);

    // 存储到全局单例
    // Store in global singleton
    *NET_STACK.lock() = Some(NetStack {
        iface,
        device,
        sockets,
    });

    // 初始化 DNS 支持 — 配置 DNS 服务器到 Interface
    // Initialize DNS support — configure DNS servers on the Interface
    init_dns();

    crate::println!("[net-stack] Network stack initialized successfully");
}

// ============================================================================
// DNS 初始化 / DNS Initialization
// ============================================================================

/// 初始化 DNS 支持
///
/// 🟡 微内核适配：在 NetStack 中创建一个持久的 DNS socket，
/// 配置 DNS 服务器地址。DNS socket 在网络栈生命周期内保持活跃。
///
/// # 调用时机
///
/// 在 NetStack 初始化完成后调用。此时文件系统可能尚未挂载，
/// 因此 resolv.conf 读取可能失败，这是正常的。
/// 后续可以通过 `reload_dns_config()` 重新加载。
///
/// Initialize DNS support by creating a persistent DNS socket.
fn init_dns() {
    let dns_servers = super::dns::get_dns_servers();

    let dns_addrs: Vec<IpAddress> = dns_servers
        .iter()
        .map(|s| IpAddress::Ipv4(*s))
        .collect();

    let mut guard = NET_STACK.lock();
    if let Some(stack) = guard.as_mut() {
        // 创建 DNS socket 并添加到 SocketSet
        // Create DNS socket and add to SocketSet
        // smoltcp 的 DNS socket 在创建时接收 DNS 服务器列表
        // smoltcp's DNS socket receives DNS server list at creation time
        let dns_socket = socket::dns::Socket::new(&dns_addrs, vec![]);
        let _dns_handle = stack.sockets.add(dns_socket);

        crate::println!(
            "[net-stack] DNS socket created with servers: {:?}",
            dns_servers
        );
    }
}

/// 重新加载 DNS 配置
///
/// 🟡 微内核适配：在文件系统挂载后调用，重新从 `/etc/resolv.conf`
/// 读取 DNS 配置。如果 resolv.conf 存在且包含有效的 nameserver，
/// 则重新创建 DNS socket。
///
/// Reload DNS configuration from `/etc/resolv.conf`.
/// Call this after the filesystem is mounted.
pub fn reload_dns_config() {
    let dns_servers = super::dns::get_dns_servers();

    let dns_addrs: Vec<IpAddress> = dns_servers
        .iter()
        .map(|s| IpAddress::Ipv4(*s))
        .collect();

    let mut guard = NET_STACK.lock();
    if let Some(stack) = guard.as_mut() {
        // 创建新的 DNS socket（旧的会在 SocketSet 中被替换）
        // Create new DNS socket (old one will be replaced in SocketSet)
        let dns_socket = socket::dns::Socket::new(&dns_addrs, vec![]);
        let _dns_handle = stack.sockets.add(dns_socket);
        crate::println!(
            "[net-stack] DNS servers reloaded: {:?}",
            dns_servers
        );
    }
}

// ============================================================================
// 网络轮询 / Network Polling
// ============================================================================

/// 轮询网络栈 — 处理收发数据包和协议状态机
///
/// 🟡 微内核适配：事件驱动网络轮询的核心函数。
/// 调用 smoltcp 的 `iface.poll()` 处理：
/// - 接收数据包（从 VirtIO-net → smoltcp 协议栈）
/// - 发送数据包（smoltcp 协议栈 → VirtIO-net）
/// - ARP 解析和缓存更新
/// - TCP 状态机推进（重传、超时、keepalive 等）
/// - UDP 数据报处理
///
/// # 调用时机
///
/// - VirtIO-net 中断处理中（事件驱动）
/// - 定时器中断中（周期性兜底）
/// - socket 系统调用前后（主动轮询）
///
/// # 返回值
///
/// 返回 `true` 表示有网络事件被处理，`false` 表示无事件。
///
/// Poll the network stack — process send/receive packets and protocol state machines.
/// Returns true if any network events were processed.
pub fn poll_net() -> bool {
    let mut guard = NET_STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return false, // 网络栈未初始化 / Network stack not initialized
    };

    let timestamp = current_timestamp();
    stack.iface.poll(timestamp, &mut stack.device, &mut stack.sockets)
}

pub fn poll_net_nonblocking() -> bool {
    let mut guard = match NET_STACK.try_lock() {
        Some(guard) => guard,
        None => return false,
    };
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return false,
    };

    let timestamp = current_timestamp();
    stack.iface.poll(timestamp, &mut stack.device, &mut stack.sockets)
}

/// 获取 smoltcp 下次需要轮询的时间
///
/// smoltcp 内部维护定时器（TCP 重传、ARP 超时等），
/// 返回下次需要调用 `poll_net()` 的时间戳。
/// 调用者可以据此设置定时器，避免不必要的轮询。
///
/// Get the next poll time from smoltcp.
/// Returns the timestamp when the next poll should occur.
pub fn poll_delay() -> Option<smoltcp::time::Duration> {
    let mut guard = NET_STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    stack.iface.poll_delay(current_timestamp(), &stack.sockets)
}

// ============================================================================
// Socket 管理 / Socket Management
// ============================================================================

/// 添加 TCP socket 到网络栈
///
/// 创建一个新的 smoltcp TCP socket 并添加到 SocketSet 中。
/// 返回 SocketHandle，用于后续操作（bind/connect/send/recv）。
///
/// Add a TCP socket to the network stack.
/// Returns a SocketHandle for subsequent operations.
pub fn add_tcp_socket() -> Option<SocketHandle> {
    let mut guard = NET_STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    let rx_buf = socket::tcp::SocketBuffer::new(vec![0u8; TCP_RX_BUF_SIZE]);
    let tx_buf = socket::tcp::SocketBuffer::new(vec![0u8; TCP_TX_BUF_SIZE]);
    let tcp_socket = socket::tcp::Socket::new(rx_buf, tx_buf);
    let handle = stack.sockets.add(tcp_socket);
    USER_SOCKET_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    Some(handle)
}

/// 添加 UDP socket 到网络栈
///
/// 创建一个新的 smoltcp UDP socket 并添加到 SocketSet 中。
/// 返回 SocketHandle，用于后续操作（bind/sendto/recvfrom）。
///
/// Add a UDP socket to the network stack.
/// Returns a SocketHandle for subsequent operations.
pub fn add_udp_socket() -> Option<SocketHandle> {
    let mut guard = NET_STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    let rx_buf = socket::udp::PacketBuffer::new(
        vec![socket::udp::PacketMetadata::EMPTY; UDP_RX_META_COUNT],
        vec![0u8; UDP_RX_BUF_SIZE],
    );
    let tx_buf = socket::udp::PacketBuffer::new(
        vec![socket::udp::PacketMetadata::EMPTY; UDP_TX_META_COUNT],
        vec![0u8; UDP_TX_BUF_SIZE],
    );
    let udp_socket = socket::udp::Socket::new(rx_buf, tx_buf);
    let handle = stack.sockets.add(udp_socket);
    USER_SOCKET_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    Some(handle)
}

/// 添加 ICMP socket 到网络栈
///
/// 创建一个新的 smoltcp ICMP socket 并添加到 SocketSet 中。
/// 用于实现 ping（ICMP echo request/reply）。
///
/// Add an ICMP socket to the network stack.
/// Returns a SocketHandle for ping operations.
pub fn add_icmp_socket() -> Option<SocketHandle> {
    let mut guard = NET_STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    let rx_buf = socket::icmp::PacketBuffer::new(
        vec![socket::icmp::PacketMetadata::EMPTY; 8],
        vec![0u8; 4096],
    );
    let tx_buf = socket::icmp::PacketBuffer::new(
        vec![socket::icmp::PacketMetadata::EMPTY; 8],
        vec![0u8; 4096],
    );
    let icmp_socket = socket::icmp::Socket::new(rx_buf, tx_buf);
    let handle = stack.sockets.add(icmp_socket);
    USER_SOCKET_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    Some(handle)
}

/// 添加 Raw socket 到网络栈
///
/// Add a raw IP socket to the network stack.
pub fn add_raw_socket(ip_version: smoltcp::wire::IpVersion, ip_protocol: smoltcp::wire::IpProtocol) -> Option<SocketHandle> {
    let mut guard = NET_STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    let rx_buf = socket::raw::PacketBuffer::new(
        vec![socket::raw::PacketMetadata::EMPTY; 8],
        vec![0u8; 16384],
    );
    let tx_buf = socket::raw::PacketBuffer::new(
        vec![socket::raw::PacketMetadata::EMPTY; 8],
        vec![0u8; 16384],
    );
    let raw_socket = socket::raw::Socket::new(ip_version, ip_protocol, rx_buf, tx_buf);
    let handle = stack.sockets.add(raw_socket);
    USER_SOCKET_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    Some(handle)
}

/// 移除 socket
///
/// 从 SocketSet 中移除指定的 socket。
///
/// Remove a socket from the socket set.
pub fn remove_socket(handle: SocketHandle) {
    let mut guard = NET_STACK.lock();
    if let Some(stack) = guard.as_mut() {
        stack.sockets.remove(handle);
        USER_SOCKET_COUNT.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// 对网络栈执行操作（带锁访问）
///
/// 提供一个安全的方式来访问 NetStack 内部的 socket，
/// 避免外部代码直接持有 SpinLock guard。
///
/// # 用法
///
/// ```rust,ignore
/// net_stack::with_socket_set(|sockets, iface| {
///     let tcp = sockets.get_mut::<socket::tcp::Socket>(handle);
///     tcp.connect(remote_endpoint, local_endpoint)?;
///     Ok(())
/// });
/// ```
///
/// Execute an operation on the network stack (with lock held).
pub fn with_socket_set<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut SocketSet<'static>, &mut Interface) -> R,
{
    let mut guard = NET_STACK.lock();
    let stack = guard.as_mut()?;
    Some(f(&mut stack.sockets, &mut stack.iface))
}

/// Access the interface and device (for IGMP join/leave multicast groups).
pub fn with_iface<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut Interface, &mut SmolVirtioNet) -> R,
{
    let mut guard = NET_STACK.lock();
    let stack = guard.as_mut()?;
    Some(f(&mut stack.iface, &mut stack.device))
}

/// 检查网络栈是否已初始化
///
/// Check if the network stack has been initialized.
pub fn is_initialized() -> bool {
    NET_INITIALIZED.load(core::sync::atomic::Ordering::Acquire)
}

// ============================================================================
// 中断驱动轮询 / Interrupt-Driven Polling
// ============================================================================

/// VirtIO-net 中断处理中的网络轮询
///
/// 🟡 微内核适配：在 VirtIO-net 中断处理函数中调用此函数，
/// 实现事件驱动的网络轮询。当 VirtIO-net 设备收到数据包或
/// 发送完成时，触发中断，中断处理函数调用此函数推进协议栈。
///
/// # 注意
///
/// 此函数在中断上下文中调用，必须快速返回。
/// SpinLock 在中断上下文中是安全的（不会死锁，因为中断处理
/// 期间同一 CPU 不会再次进入中断处理）。
///
/// Handle network polling triggered by VirtIO-net interrupt.
/// Called from the VirtIO-net interrupt handler.
pub fn on_net_interrupt() {
    // RV/LA 的外部中断入口对**所有**设备中断（含 virtio-blk）都会调到这里；
    // 无用户 socket 时 smoltcp 内部没有任何待推进的协议状态，直接短路，
    // 免得磁盘密集负载的每次 blk 中断都付一次完整 iface.poll（poll_at +
    // Instant u128 换算是 libc-bench 期间 ~300M 指令的纯空转）。
    if USER_SOCKET_COUNT.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        return;
    }
    if poll_net_nonblocking() {
        // 网络栈状态发生变化，唤醒等待数据的 AF_INET socket
        wake_inet_sockets();
    }
}

/// 唤醒所有 AF_INET socket 的等待者
///
/// 在 poll_net 处理完 incoming packets 后调用：
/// - 有数据可读 → wake readers（unblock recv/accept）
/// - TCP 状态变化 → wake all（unblock connect）
/// 对标 Linux sk_data_ready / sk_state_change callbacks
fn wake_inet_sockets() {
    use super::socket::all_socket_files;
    use super::backend;
    use smoltcp::socket::tcp as smol_tcp;

    for socket in all_socket_files() {
        let (is_inet, handle, is_tcp) = socket.with_state(|state| {
            let is_inet = state.domain == 2 && state.backend.smoltcp().is_some();
            let handle = state.backend.smoltcp();
            let is_tcp = state.backend.is_tcp();
            (is_inet, handle, is_tcp)
        });
        if !is_inet {
            continue;
        }

        // 对 TCP socket：状态变化也应唤醒（connect 完成 / RST / FIN）
        if is_tcp {
            if let Some(h) = handle {
                let (should_wake_read, should_wake_write) = with_socket_set(|sockets, _iface| {
                    let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(h);
                    let wake_read = tcp_socket.can_recv()
                        || tcp_socket.state() == smol_tcp::State::Established
                        || tcp_socket.state() == smol_tcp::State::Closed
                        || tcp_socket.state() == smol_tcp::State::CloseWait
                        || !tcp_socket.is_active();
                    let wake_write = tcp_socket.can_send()
                        || tcp_socket.state() == smol_tcp::State::Established;
                    (wake_read, wake_write)
                }).unwrap_or((false, false));
                if should_wake_read {
                    socket.wake_readers();
                }
                if should_wake_write {
                    socket.wake_writers();
                }
            }
        } else if backend::read_ready(&socket) {
            socket.wake_readers();
        }
    }
}

/// 定时器中断中的网络轮询
///
/// 🟡 微内核适配：在定时器中断处理中周期性调用此函数，
/// 作为事件驱动轮询的兜底机制。确保即使 VirtIO-net 中断
/// 丢失或延迟，网络栈仍能正常推进。
///
/// 建议调用频率：每 10-100ms 一次（取决于网络负载）。
///
/// Periodic network polling from timer interrupt.
/// Acts as a fallback for interrupt-driven polling.
/// 定时器兜底轮询的下次到期时刻（smoltcp 毫秒时间戳）。
/// 真正的收发由 VirtIO 中断（on_net_interrupt）和 socket 系统调用路径驱动，
/// 定时器只兜 smoltcp 内部定时器（TCP 重传/ARP 超时等）——按 poll_delay
/// 的到期点轮询即可，空闲时免去每 tick 一次完整 iface.poll 的固定开销。
static NEXT_TIMER_POLL_MS: core::sync::atomic::AtomicI64 =
    core::sync::atomic::AtomicI64::new(0);

/// 当前存活的用户 socket 数（add_*_socket / remove_socket 维护）。
/// 为 0 时 smoltcp 内部无任何待推进的协议状态（无重传/无待发），
/// 定时器兕底轮询直接短路；真实收包仍由 VirtIO 中断（on_net_interrupt）
/// 驱动。避免无网络负载（libc-bench 等）时每次 hrtimer 中断都付一次
/// 完整 iface.poll + 时间换算（u128 除法）的固定开销。
static USER_SOCKET_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// 兜底轮询间隔上限（毫秒）：即使 poll_delay 报告"无事可做"，也至少
/// 每隔这么久强制 poll 一次，防中断丢失导致协议栈停摆。
const TIMER_POLL_CAP_MS: i64 = 20;

pub fn on_timer_tick() {
    // 仅在网络栈已初始化且存在用户 socket 时轮询
    if !is_initialized() {
        return;
    }
    if USER_SOCKET_COUNT.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        return;
    }

    let now_ms = current_timestamp().total_millis();
    if now_ms < NEXT_TIMER_POLL_MS.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }

    // 中断上下文：全程 try_lock（拿不到锁下个 tick 再试，不推迟到期点）。
    let polled = {
        let mut guard = match NET_STACK.try_lock() {
            Some(g) => g,
            None => return,
        };
        let stack = match guard.as_mut() {
            Some(s) => s,
            None => return,
        };
        let ts = current_timestamp();
        let changed = stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);
        let delay_ms = stack
            .iface
            .poll_delay(ts, &stack.sockets)
            .map(|d| (d.total_millis() as i64).min(TIMER_POLL_CAP_MS))
            .unwrap_or(TIMER_POLL_CAP_MS);
        NEXT_TIMER_POLL_MS.store(
            now_ms + delay_ms.max(1),
            core::sync::atomic::Ordering::Relaxed,
        );
        changed
    };
    if polled {
        wake_inet_sockets();
    }
}

// ============================================================================
// 时间戳工具 / Timestamp Utility
// ============================================================================

/// 获取当前时间戳（smoltcp 格式）
///
/// 将内核的计时器 tick 转换为 smoltcp 需要的毫秒级时间戳。
/// 使用 HAL trait 的 `timer_get_time()` 和 `timer_freq()` 计算。
///
/// Get current timestamp in smoltcp format (milliseconds).
fn current_timestamp() -> Instant {
    let ticks = crate::arch::asm_helpers::rdtime();
    let freq = crate::arch::consts::CLOCK_FREQ as u64;

    if freq == 0 {
        return Instant::from_millis(0);
    }

    // 转换为毫秒：ticks * 1000 / freq
    // 避免溢出：先除后乘
    // Convert to milliseconds: ticks * 1000 / freq
    // Avoid overflow: divide first, then multiply
    let secs = ticks / freq;
    let remainder = ticks % freq;
    let millis = secs * 1000 + remainder * 1000 / freq;

    Instant::from_millis(millis as i64)
}

// ============================================================================
// 调试和诊断 / Debug and Diagnostics
// ============================================================================

/// 打印网络栈状态信息
///
/// 用于调试和诊断，输出当前网络接口配置和 socket 状态。
///
/// Print network stack status information for debugging.
pub fn print_status() {
    let guard = NET_STACK.lock();
    let stack = match guard.as_ref() {
        Some(s) => s,
        None => {
            crate::println!("[net-stack] Not initialized");
            return;
        }
    };

    crate::println!("[net-stack] === Network Stack Status ===");

    // 打印 IP 地址
    // Print IP addresses
    for cidr in stack.iface.ip_addrs() {
        crate::println!("[net-stack] IP: {}", cidr);
    }

    // 打印 socket 数量（通过遍历计数）
    // Print socket count
    // 注意：smoltcp 0.11 的 SocketSet 没有直接的 len() 方法
    // Note: smoltcp 0.11 SocketSet doesn't have a direct len() method
    crate::println!("[net-stack] Network stack is active");
    crate::println!("[net-stack] ========================");
}

/// 网络栈自测试
///
/// 验证网络栈初始化是否成功，IP 配置是否正确。
///
/// Network stack self-test.
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[net-stack] Running self-test...");

    // 测试初始化状态
    if !is_initialized() {
        crate::println!("[net-stack] WARNING: Network stack not initialized (VirtIO-net may not be available)");
        return Ok(());
    }

    // 测试轮询
    let _ = poll_net();

    // 测试 socket 创建和删除
    if let Some(tcp_handle) = add_tcp_socket() {
        crate::println!("[net-stack] TCP socket created successfully");
        remove_socket(tcp_handle);
        crate::println!("[net-stack] TCP socket removed successfully");
    }

    if let Some(udp_handle) = add_udp_socket() {
        crate::println!("[net-stack] UDP socket created successfully");
        remove_socket(udp_handle);
        crate::println!("[net-stack] UDP socket removed successfully");
    }

    // 打印状态
    print_status();

    crate::println!("[net-stack] Self-test passed!");
    Ok(())
}
