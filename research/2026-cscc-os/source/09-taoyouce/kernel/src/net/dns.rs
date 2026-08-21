//! DNS 解析模块 — 基于 smoltcp DNS socket 实现域名解析
//!
//! 🟢 直接复用：smoltcp 的 DNS socket 功能（socket-dns feature）
//! 🟡 微内核适配：支持从 `/etc/resolv.conf` 读取 DNS 配置，
//!    支持 QEMU LoongArch64 端口转发参数
//!
//! 来源: 适配层代码为 RustMicroOS 原创
//! 复用: smoltcp crate (BSD-0-Clause License, https://github.com/smoltcp-rs/smoltcp)
//!
//! # 设计说明
//!
//! DNS 解析通过 smoltcp 内置的 DNS socket 实现，支持：
//! - 默认使用 QEMU user-mode networking 的 DNS 服务器（10.0.2.3）
//! - 可从 `/etc/resolv.conf` 读取自定义 DNS 配置
//! - 阻塞式域名解析（轮询等待 DNS 响应）
//! - 支持 git clone 等需要域名解析的网络操作
//!
//! # 参考
//!
//! - ArceOS 项目的 DNS 解析实现: `axnet/src/smoltcp_impl/dns.rs`
//! - smoltcp DNS socket 文档: https://docs.rs/smoltcp/0.11/smoltcp/socket/dns/

#![allow(dead_code)]

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::wire::{IpAddress, Ipv4Address};

// ============================================================================
// DNS 配置常量 / DNS Configuration Constants
// ============================================================================

/// 默认 DNS 服务器 — QEMU user-mode networking 的 DNS 地址
/// Default DNS server — QEMU user-mode networking DNS address
const DEFAULT_DNS_SERVER: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

/// DNS 查询超时时间（毫秒）
/// DNS query timeout in milliseconds
const DNS_QUERY_TIMEOUT_MS: u64 = 5000;

/// DNS 查询最大轮询次数
/// Maximum poll iterations for DNS query
const DNS_MAX_POLL_ITERATIONS: usize = 500;

/// DNS 轮询间隔（毫秒）— 每次轮询之间的等待时间
/// DNS poll interval in milliseconds — wait time between each poll
const DNS_POLL_INTERVAL_MS: u64 = 10;

// ============================================================================
// resolv.conf 解析 / resolv.conf Parsing
// ============================================================================

/// 从 `/etc/resolv.conf` 文件内容解析 DNS 服务器地址列表
///
/// 🟡 微内核适配：解析标准 Linux resolv.conf 格式，
/// 提取 `nameserver` 行中的 IPv4 地址。
///
/// # 格式
///
/// ```text
/// # 注释行
/// nameserver 10.0.2.3
/// nameserver 8.8.8.8
/// ```
///
/// # 参数
///
/// - `content`: resolv.conf 文件的文本内容
///
/// # 返回值
///
/// 解析出的 DNS 服务器 IPv4 地址列表。如果解析失败或无有效条目，
/// 返回空列表。
///
/// Parse DNS server addresses from `/etc/resolv.conf` content.
/// Returns a list of IPv4 addresses found in `nameserver` lines.
pub fn parse_resolv_conf(content: &str) -> Vec<Ipv4Address> {
    let mut servers = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        // 跳过空行和注释行
        // Skip empty lines and comment lines
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // 解析 "nameserver <ip>" 行
        // Parse "nameserver <ip>" lines
        if let Some(addr_str) = line.strip_prefix("nameserver") {
            let addr_str = addr_str.trim();
            if let Some(addr) = parse_ipv4(addr_str) {
                servers.push(addr);
                log::info!("[dns] Parsed nameserver from resolv.conf: {}", addr);
            } else {
                log::warn!("[dns] Invalid nameserver address in resolv.conf: {}", addr_str);
            }
        }
    }

    servers
}

/// 尝试从文件系统读取 `/etc/resolv.conf` 并解析 DNS 服务器
///
/// 🟡 微内核适配：通过内核 VFS 读取 `/etc/resolv.conf` 文件。
/// 如果文件不存在或读取失败，返回空列表（调用者应使用默认 DNS）。
///
/// Try to read and parse `/etc/resolv.conf` from the filesystem.
/// Returns an empty list if the file doesn't exist or can't be read.
pub fn read_resolv_conf() -> Vec<Ipv4Address> {
    // 尝试通过 VFS 查找 /etc/resolv.conf
    // Try to find /etc/resolv.conf via VFS
    let inode = match crate::fs::lookup("/etc/resolv.conf") {
        Ok(inode) => inode,
        Err(_) => {
            // 文件不存在或读取失败，这是正常情况（使用默认 DNS）
            // File doesn't exist or read failed, this is normal (use default DNS)
            log::info!("[dns] /etc/resolv.conf not found, using default DNS server");
            return Vec::new();
        }
    };

    // 获取文件接口并读取内容
    // Get file interface and read content
    let file = match inode.as_file() {
        Some(f) => f,
        None => {
            log::warn!("[dns] /etc/resolv.conf is not a regular file");
            return Vec::new();
        }
    };

    // 读取文件内容（resolv.conf 通常很小，4KB 足够）
    // Read file content (resolv.conf is usually small, 4KB is enough)
    let mut buf = alloc::vec![0u8; 4096];
    let bytes_read = match file.read(0, &mut buf) {
        Ok(n) => n,
        Err(e) => {
            log::warn!("[dns] Failed to read /etc/resolv.conf: {:?}", e);
            return Vec::new();
        }
    };

    // 将字节数据转换为 UTF-8 字符串并解析
    // Convert byte data to UTF-8 string and parse
    match core::str::from_utf8(&buf[..bytes_read]) {
        Ok(content) => {
            let servers = parse_resolv_conf(content);
            if servers.is_empty() {
                log::info!("[dns] /etc/resolv.conf found but no valid nameservers");
            } else {
                log::info!("[dns] Loaded {} DNS server(s) from /etc/resolv.conf", servers.len());
            }
            servers
        }
        Err(_) => {
            log::warn!("[dns] /etc/resolv.conf contains invalid UTF-8");
            Vec::new()
        }
    }
}

/// 获取 DNS 服务器列表（优先 resolv.conf，回退到默认值）
///
/// 优先级：
/// 1. `/etc/resolv.conf` 中配置的 DNS 服务器
/// 2. QEMU user-mode networking 默认 DNS（10.0.2.3）
///
/// Get DNS server list (prefer resolv.conf, fallback to default).
pub fn get_dns_servers() -> Vec<Ipv4Address> {
    let servers = read_resolv_conf();
    if servers.is_empty() {
        log::info!("[dns] Using default DNS server: {}", DEFAULT_DNS_SERVER);
        alloc::vec![DEFAULT_DNS_SERVER]
    } else {
        servers
    }
}

// ============================================================================
// DNS 解析 API / DNS Resolution API
// ============================================================================

/// 解析域名为 IPv4 地址（阻塞式）
///
/// 🟢 直接复用 smoltcp 的 DNS socket 功能。
/// 通过 smoltcp 的 DNS socket 发送 DNS 查询，轮询等待响应。
///
/// # 参数
///
/// - `hostname`: 要解析的域名（如 "github.com"）
///
/// # 返回值
///
/// - `Ok(Ipv4Address)`: 解析成功，返回第一个 IPv4 地址
/// - `Err(DnsError)`: 解析失败
///
/// # 示例
///
/// ```rust,ignore
/// let ip = dns::resolve("github.com")?;
/// // ip = Ipv4Address::new(20, 205, 243, 166)
/// ```
///
/// Resolve a hostname to an IPv4 address (blocking).
/// Uses smoltcp's DNS socket to send queries and poll for responses.
pub fn resolve(hostname: &str) -> Result<Ipv4Address, DnsError> {
    log::info!("[dns] Resolving hostname: {}", hostname);

    // 检查网络栈是否已初始化
    // Check if network stack is initialized
    if !super::net_stack::is_initialized() {
        log::error!("[dns] Network stack not initialized");
        return Err(DnsError::NetworkNotReady);
    }

    // 获取 DNS 服务器列表
    // Get DNS server list
    let dns_servers = get_dns_servers();
    let dns_server_addrs: Vec<IpAddress> = dns_servers
        .iter()
        .map(|s| IpAddress::Ipv4(*s))
        .collect();

    // 在网络栈中创建 DNS socket 并发起查询
    // Create DNS socket in network stack and start query
    let query_handle = super::net_stack::with_socket_set(|sockets, iface| {
        // 创建 DNS socket（传入 DNS 服务器列表）
        // Create DNS socket (pass DNS server list)
        let dns_socket = smoltcp::socket::dns::Socket::new(&dns_server_addrs, vec![]);
        let handle = sockets.add(dns_socket);

        // 发起 DNS 查询
        // Start DNS query
        let dns = sockets.get_mut::<smoltcp::socket::dns::Socket>(handle);
        let query = dns.start_query(iface.context(), hostname, smoltcp::wire::DnsQueryType::A);

        match query {
            Ok(qh) => Ok((handle, qh)),
            Err(e) => {
                log::error!("[dns] Failed to start DNS query for '{}': {:?}", hostname, e);
                sockets.remove(handle);
                Err(DnsError::QueryFailed)
            }
        }
    });

    let (socket_handle, query_handle) = match query_handle {
        Some(Ok(handles)) => handles,
        Some(Err(e)) => return Err(e),
        None => return Err(DnsError::NetworkNotReady),
    };

    // 轮询等待 DNS 响应
    // Poll and wait for DNS response
    let result = poll_dns_response(socket_handle, query_handle, hostname);

    // 清理 DNS socket
    // Clean up DNS socket
    super::net_stack::remove_socket(socket_handle);

    result
}

/// 轮询等待 DNS 响应
///
/// 反复调用 `poll_net()` 推进网络栈，检查 DNS 查询是否完成。
/// 超时后返回错误。
///
/// Poll for DNS response by repeatedly calling poll_net().
/// Returns error on timeout.
fn poll_dns_response(
    socket_handle: smoltcp::iface::SocketHandle,
    query_handle: smoltcp::socket::dns::QueryHandle,
    hostname: &str,
) -> Result<Ipv4Address, DnsError> {
    use smoltcp::socket::dns;

    for i in 0..DNS_MAX_POLL_ITERATIONS {
        // 轮询网络栈
        // Poll the network stack
        super::net_stack::poll_net();

        // 检查 DNS 查询结果
        // Check DNS query result
        let result = super::net_stack::with_socket_set(|sockets, _iface| {
            let dns_socket = sockets.get_mut::<dns::Socket>(socket_handle);
            dns_socket.get_query_result(query_handle)
        });

        match result {
            Some(Ok(addrs)) => {
                // DNS 查询成功，提取第一个 IPv4 地址
                // DNS query succeeded, extract first IPv4 address
                for addr in addrs.iter() {
                    if let IpAddress::Ipv4(v4) = addr {
                        log::info!(
                            "[dns] Resolved '{}' -> {} (after {} polls)",
                            hostname, v4, i + 1
                        );
                        return Ok(*v4);
                    }
                }
                log::warn!("[dns] DNS response for '{}' contains no IPv4 addresses", hostname);
                return Err(DnsError::NoIpv4Address);
            }
            Some(Err(dns::GetQueryResultError::Pending)) => {
                // 查询仍在进行中，继续轮询
                // Query still pending, continue polling
                // 简单的忙等待（微内核中可以改为 yield）
                // Simple busy-wait (can be changed to yield in microkernel)
                for _ in 0..10000 {
                    core::hint::spin_loop();
                }
            }
            Some(Err(dns::GetQueryResultError::Failed)) => {
                log::error!("[dns] DNS query failed for '{}'", hostname);
                return Err(DnsError::QueryFailed);
            }
            None => {
                return Err(DnsError::NetworkNotReady);
            }
        }
    }

    log::error!(
        "[dns] DNS query timed out for '{}' after {} iterations",
        hostname, DNS_MAX_POLL_ITERATIONS
    );
    Err(DnsError::Timeout)
}

// ============================================================================
// 辅助函数 / Helper Functions
// ============================================================================

/// 解析 IPv4 地址字符串（如 "10.0.2.3"）
///
/// Parse an IPv4 address string (e.g., "10.0.2.3").
fn parse_ipv4(s: &str) -> Option<Ipv4Address> {
    let s = s.trim();
    let mut octets = [0u8; 4];
    let mut parts = s.split('.');
    for octet in octets.iter_mut() {
        let part = parts.next()?;
        *octet = part.parse::<u8>().ok()?;
    }
    // 确保没有多余的部分
    // Ensure no extra parts
    if parts.next().is_some() {
        return None;
    }
    Some(Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]))
}

// ============================================================================
// 错误类型 / Error Types
// ============================================================================

/// DNS 解析错误类型
/// DNS resolution error type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    /// 网络栈未初始化
    /// Network stack not initialized
    NetworkNotReady,
    /// DNS 查询失败（服务器无响应或返回错误）
    /// DNS query failed (server unreachable or returned error)
    QueryFailed,
    /// DNS 查询超时
    /// DNS query timed out
    Timeout,
    /// DNS 响应中无 IPv4 地址
    /// No IPv4 address in DNS response
    NoIpv4Address,
    /// 无效的域名
    /// Invalid hostname
    InvalidHostname,
}

impl core::fmt::Display for DnsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DnsError::NetworkNotReady => write!(f, "network stack not initialized"),
            DnsError::QueryFailed => write!(f, "DNS query failed"),
            DnsError::Timeout => write!(f, "DNS query timed out"),
            DnsError::NoIpv4Address => write!(f, "no IPv4 address in DNS response"),
            DnsError::InvalidHostname => write!(f, "invalid hostname"),
        }
    }
}

// ============================================================================
// 端口转发配置 / Port Forwarding Configuration
// ============================================================================

/// QEMU 端口转发配置
///
/// 🟡 微内核适配：QEMU user-mode networking 支持端口转发，
/// 允许 host 通过指定端口访问 guest 的网络服务。
///
/// 对于 LoongArch64 QEMU，评测系统可能使用如下参数：
/// ```text
/// -netdev user,id=net,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555
/// ```
///
/// 端口转发在 QEMU 侧配置，guest 内核无需特殊处理。
/// guest 只需正常监听对应端口即可。
///
/// # 注意
///
/// 端口转发是 QEMU 的功能，不需要内核代码支持。
/// 此模块仅提供文档说明和配置常量，供 Makefile/启动脚本使用。
///
/// QEMU port forwarding configuration.
/// Port forwarding is a QEMU feature, no kernel code changes needed.
/// The guest kernel just needs to listen on the forwarded ports normally.
pub struct PortForwardConfig {
    /// 主机端口
    /// Host port
    pub host_port: u16,
    /// Guest 端口
    /// Guest port
    pub guest_port: u16,
    /// 协议类型（"tcp" 或 "udp"）
    /// Protocol type ("tcp" or "udp")
    pub protocol: &'static str,
}

/// 默认端口转发配置 — iperf 测试使用的端口
/// Default port forwarding config — ports used by iperf testing
pub const DEFAULT_PORT_FORWARDS: &[PortForwardConfig] = &[
    PortForwardConfig {
        host_port: 5555,
        guest_port: 5555,
        protocol: "tcp",
    },
    PortForwardConfig {
        host_port: 5555,
        guest_port: 5555,
        protocol: "udp",
    },
    PortForwardConfig {
        host_port: 5201,
        guest_port: 5201,
        protocol: "tcp",
    },
    PortForwardConfig {
        host_port: 5201,
        guest_port: 5201,
        protocol: "udp",
    },
];

/// 生成 QEMU 端口转发参数字符串
///
/// 用于构建 QEMU 启动命令中的 `-netdev user,...,hostfwd=...` 参数。
///
/// # 返回值
///
/// 返回格式如 "hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555" 的字符串
///
/// Generate QEMU port forwarding argument string.
pub fn qemu_hostfwd_args() -> String {
    let mut args = String::new();
    for (i, pf) in DEFAULT_PORT_FORWARDS.iter().enumerate() {
        if i > 0 {
            args.push(',');
        }
        args.push_str(&alloc::format!(
            "hostfwd={}::{}-:{}",
            pf.protocol, pf.host_port, pf.guest_port
        ));
    }
    args
}
