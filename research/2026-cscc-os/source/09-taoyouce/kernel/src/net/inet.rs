//! AF_INET 真实网络路径 — 对接 smoltcp 协议栈
//!
//! 将 AF_INET TCP/UDP socket 操作路由到 smoltcp 真实 TCP/IP 协议栈，
//! 不再走 loopback 内存 buffer 拷贝。
//!
//! 设计：
//! - bind/listen/accept/connect/send/recv 全部通过 `net_stack::with_socket_set`
//!   操作 smoltcp SocketSet 中的 TCP/UDP socket
//! - 阻塞时返回 WouldBlock，由上层 syscall 层 suspend+poll 唤醒
//! - loopback (127.0.0.1) 地址也走 smoltcp 协议栈（smoltcp 内部处理）
//!
//! Reference: Linux net/ipv4/tcp.c, net/ipv4/udp.c, net/ipv4/af_inet.c

use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::fs::{File, READINESS_READ};
use smoltcp::socket::tcp as smol_tcp;
use smoltcp::socket::udp as smol_udp;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};

use super::net_stack;
use super::socket::{SocketError, SocketFile};

fn current_pid() -> Option<usize> {
    let pid = crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 { None } else { Some(pid) }
}

// ============================================================================
// 地址转换工具 — sockaddr_in <-> smoltcp IpEndpoint
// ============================================================================

/// 从内核 16 字节 sockaddr 表示提取 IPv4 endpoint
/// 格式: [family_lo, family_hi, port_hi, port_lo, ip0, ip1, ip2, ip3, ...]
fn addr_to_endpoint(addr: &[u8; 16]) -> IpEndpoint {
    let port = u16::from_be_bytes([addr[2], addr[3]]);
    let ip = Ipv4Address::new(addr[4], addr[5], addr[6], addr[7]);
    IpEndpoint::new(IpAddress::Ipv4(ip), port)
}

/// 从内核 16 字节 sockaddr 提取 listen endpoint（允许 0.0.0.0 = any）
fn addr_to_listen_endpoint(addr: &[u8; 16]) -> IpListenEndpoint {
    let port = u16::from_be_bytes([addr[2], addr[3]]);
    let ip = Ipv4Address::new(addr[4], addr[5], addr[6], addr[7]);
    let addr = if ip == Ipv4Address::UNSPECIFIED {
        None
    } else {
        Some(IpAddress::Ipv4(ip))
    };
    IpListenEndpoint { addr, port }
}

/// 将 smoltcp IpEndpoint 写回内核 16 字节 sockaddr 格式
fn endpoint_to_addr(ep: IpEndpoint) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[0] = 2; // AF_INET
    addr[1] = 0;
    let port_bytes = ep.port.to_be_bytes();
    addr[2] = port_bytes[0];
    addr[3] = port_bytes[1];
    let IpAddress::Ipv4(ipv4) = ep.addr;
    let octets = ipv4.0;
    addr[4] = octets[0];
    addr[5] = octets[1];
    addr[6] = octets[2];
    addr[7] = octets[3];
    addr
}

// ============================================================================
// TCP 操作 — per Linux net/ipv4/tcp.c / inet_stream_ops
// ============================================================================

/// TCP bind — 绑定本地地址+端口
pub fn tcp_bind(_socket: &Arc<SocketFile>, addr: [u8; 16], _addrlen: usize) -> Result<(), SocketError> {
    let ep = addr_to_listen_endpoint(&addr);
    // Landlock: check BIND_TCP permission
    if ep.port != 0 {
        if !crate::security::landlock::check_net_access(
            ep.port,
            crate::security::landlock::LANDLOCK_ACCESS_NET_BIND_TCP,
        ) {
            return Err(SocketError::PermissionDenied);
        }
    }
    if ep.port == 0 {
        // 自动分配端口 — 由上层 normalize_bind_addr 处理
        // 这里只记录 bind 状态，实际绑定在 listen/connect 时生效
    }
    // smoltcp TCP socket 的 bind 通过 listen() 或 connect() 的 local endpoint 实现
    // 此处仅记录绑定地址到 socket state，由上层 backend 设置 bound=true
    Ok(())
}

/// TCP listen — 将 socket 切换到 LISTEN 状态
pub fn tcp_listen(socket: &Arc<SocketFile>) -> Result<(), SocketError> {
    let (bound_addr, handle) = socket.with_state(|state| {
        (state.bound_addr, state.backend.smoltcp())
    });
    let handle = handle.ok_or(SocketError::NoResources)?;
    let listen_ep = addr_to_listen_endpoint(&bound_addr);

    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        tcp_socket.listen(listen_ep).map_err(|_| SocketError::AddrInUse)
    }).unwrap_or(Err(SocketError::NoResources))
}

/// TCP accept — 从 LISTEN socket 接受新连接
///
/// smoltcp 的 TCP accept 模型：当一个 listening socket 完成三次握手后，
/// 它自身变成 ESTABLISHED 状态。为了实现 Linux 的 accept 语义（listener
/// 保持 LISTEN，返回新的 connected socket），我们需要：
/// 1. 检查 listening socket 是否已进入 ESTABLISHED
/// 2. 如果是，创建新 socket，将 smoltcp handle 转移给新 socket
/// 3. 为 listener 创建新的 smoltcp TCP socket 重新 listen
pub fn tcp_accept(listener: &Arc<SocketFile>) -> Result<(Arc<super::socket::SocketFile>, [u8; 16]), SocketError> {
    let (bound_addr, handle, domain, sock_type, proto) = listener.with_state(|state| {
        (state.bound_addr, state.backend.smoltcp(), state.domain, state.sock_type, state.proto)
    });
    let handle = handle.ok_or(SocketError::NoResources)?;

    // 检查当前 listening socket 是否有连接到达
    let (remote_ep, is_active) = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        let active = tcp_socket.is_active();
        let remote = tcp_socket.remote_endpoint();
        (remote, active)
    }).unwrap_or((None, false));

    if !is_active || remote_ep.is_none() {
        return Err(SocketError::WouldBlock);
    }

    let remote_ep = remote_ep.unwrap();
    let peer_addr = endpoint_to_addr(remote_ep);

    // 创建新的 SocketFile 用于 accepted 连接
    let new_socket = super::socket::create_socket_file(domain, sock_type, proto, false);

    // 将当前 smoltcp handle（已 ESTABLISHED）转移给新 socket
    // 为 listener 分配新的 smoltcp TCP socket 重新 listen
    let new_handle = net_stack::add_tcp_socket();
    let new_handle = new_handle.ok_or(SocketError::NoResources)?;

    // 交换：新 socket 获得已连接的 handle，listener 获得新分配的 handle
    listener.with_state_mut(|state| {
        state.backend.swap_smoltcp_handle(new_handle);
    });
    new_socket.with_state_mut(|state| {
        state.backend.set_smoltcp_handle(handle);
        state.bound_addr = bound_addr;
        state.bound = true;
    });

    // 让 listener 的新 handle 重新 listen
    let listen_ep = addr_to_listen_endpoint(&bound_addr);
    let _ = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(new_handle);
        let _ = tcp_socket.listen(listen_ep);
    });

    Ok((new_socket, peer_addr))
}

/// TCP connect — 主动连接到远端
///
/// 对标 Linux: 阻塞 socket 的 connect() 会阻塞直到三次握手完成（ESTABLISHED）
/// 或超时（ETIMEDOUT）。非阻塞 socket 返回 EINPROGRESS。
/// 这里只负责发起连接，阻塞等待由上层 syscall loop 驱动。
pub fn tcp_connect(socket: &Arc<SocketFile>, dest: [u8; 16]) -> Result<(), SocketError> {
    let (bound_addr, handle, bound, nonblocking) = socket.with_state(|state| {
        (state.bound_addr, state.backend.smoltcp(), state.bound, state.nonblocking)
    });
    let handle = handle.ok_or(SocketError::NoResources)?;
    let remote_ep = addr_to_endpoint(&dest);

    // Landlock: check CONNECT_TCP permission
    if !crate::security::landlock::check_net_access(
        remote_ep.port,
        crate::security::landlock::LANDLOCK_ACCESS_NET_CONNECT_TCP,
    ) {
        return Err(SocketError::PermissionDenied);
    }

    // 确定本地 endpoint
    let local_ep = if bound {
        addr_to_endpoint(&bound_addr)
    } else {
        // 未 bind：使用 10.0.2.15 + 临时端口
        let port = super::socket::allocate_ephemeral_port(49152);
        IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 15)), port)
    };

    // 先 poll 确保网络栈状态最新
    net_stack::poll_net();

    let result = net_stack::with_socket_set(|sockets, iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        let cx = iface.context();
        tcp_socket.connect(cx, remote_ep, local_ep)
            .map_err(|_| SocketError::ConnectionRefused)
    }).unwrap_or(Err(SocketError::NoResources));

    if result.is_ok() {
        // 记录本地绑定地址
        if !bound {
            let local_addr = endpoint_to_addr(local_ep);
            socket.with_state_mut(|state| {
                state.bound_addr = local_addr;
                state.bound = true;
            });
        }
        // 轮询驱动 SYN 发送
        net_stack::poll_net();

        // 非阻塞模式：返回 WouldBlock (对应 Linux EINPROGRESS)
        if nonblocking {
            return Err(SocketError::WouldBlock);
        }

        // 阻塞模式：等待三次握手完成
        // 使用 loop + suspend，由 wake_inet_sockets 唤醒
        loop {
            net_stack::poll_net();
            let tcp_state = net_stack::with_socket_set(|sockets, _iface| {
                let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
                tcp_socket.state()
            });
            match tcp_state {
                Some(smol_tcp::State::Established) => return Ok(()),
                Some(smol_tcp::State::Closed) => return Err(SocketError::ConnectionRefused),
                Some(smol_tcp::State::TimeWait) => return Err(SocketError::ConnectionRefused),
                _ => {
                    // 仍在握手中 (SynSent / SynReceived)
                    // 注册 waiter 并挂起，等 poll_net 唤醒
                    if let Some(pid) = current_pid() {
                        File::register_waiter(socket.as_ref(), READINESS_READ, pid, None);
                    }
                    net_stack::poll_net();
                    let after_register = net_stack::with_socket_set(|sockets, _iface| {
                        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
                        tcp_socket.state()
                    });
                    match after_register {
                        Some(smol_tcp::State::Established) => return Ok(()),
                        Some(smol_tcp::State::Closed) => {
                            return Err(SocketError::ConnectionRefused);
                        }
                        Some(smol_tcp::State::TimeWait) => {
                            return Err(SocketError::ConnectionRefused);
                        }
                        _ => {}
                    }
                    crate::sched::process::enter_wait_channel("tcp_connect");
                    crate::sched::process::suspend_current_and_run_next();
                }
            }
        }
    }

    result
}

/// TCP connect 完成检查 — 检查是否已建立连接
pub fn tcp_is_connected(socket: &Arc<SocketFile>) -> bool {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return false };

    net_stack::poll_net();
    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        tcp_socket.state() == smol_tcp::State::Established
    }).unwrap_or(false)
}

/// TCP send — 发送数据
pub fn tcp_send(socket: &Arc<SocketFile>, data: &[u8]) -> Result<usize, SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    // 先 poll 让发送缓冲区腾出空间
    net_stack::poll_net();

    let result = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);

        if !tcp_socket.is_active() {
            return Err(SocketError::BrokenPipe);
        }

        if !tcp_socket.can_send() {
            return Err(SocketError::WouldBlock);
        }

        tcp_socket.send_slice(data).map_err(|_| SocketError::BrokenPipe)
    }).unwrap_or(Err(SocketError::NoResources));

    // 发送后立即 poll 驱动数据包发出
    if result.is_ok() {
        net_stack::poll_net();
    }

    result
}

/// TCP recv — 接收数据
pub fn tcp_recv(socket: &Arc<SocketFile>, buf: &mut [u8]) -> Result<usize, SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    // poll 先收取最新数据
    net_stack::poll_net();

    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);

        if !tcp_socket.is_active() && !tcp_socket.may_recv() {
            // 连接已关闭且无更多数据
            return Ok(0);
        }

        if !tcp_socket.may_recv() {
            return Ok(0); // FIN received, EOF
        }

        if !tcp_socket.can_recv() {
            return Err(SocketError::WouldBlock);
        }

        tcp_socket.recv_slice(buf).map_err(|_| SocketError::ConnectionReset)
    }).unwrap_or(Err(SocketError::NoResources))
}

/// TCP shutdown — 关闭连接的一端或双端
pub fn tcp_shutdown(socket: &Arc<SocketFile>, how: i32) -> Result<(), SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        match how {
            0 => { /* SHUT_RD: smoltcp 无单独关闭读端 API */ }
            1 | 2 => { tcp_socket.close(); } // SHUT_WR or SHUT_RDWR
            _ => { tcp_socket.close(); }
        }
        Ok(())
    }).unwrap_or(Err(SocketError::NoResources))
}

/// TCP read_ready — 检查是否有数据可读
pub fn tcp_read_ready(socket: &SocketFile) -> bool {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return false };

    net_stack::poll_net_nonblocking();
    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        tcp_socket.can_recv() || !tcp_socket.is_active()
    }).unwrap_or(false)
}

/// TCP accept_ready — listening socket 是否有外部(smoltcp)连接到达可 accept。
///
/// 对标 `tcp_accept` 的就绪判定（见上）：smoltcp 的 listening socket 完成三次
/// 握手后自身变为 ESTABLISHED（`is_active()`）且带有 `remote_endpoint`。仅用于
/// listener 的 accept 就绪判定——绝不能用 `tcp_read_ready`，那条对 dormant 的
/// LISTEN 句柄 `!is_active()` 恒报可读，会让 `register_waiter` 漏登记 accept
/// 等待者从而丢掉 connect() 的唤醒。环回连接的 accept 就绪由 loopback accept
/// 队列单独判定。
pub fn tcp_accept_ready(socket: &SocketFile) -> bool {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return false };

    net_stack::poll_net_nonblocking();
    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        tcp_socket.is_active() && tcp_socket.remote_endpoint().is_some()
    }).unwrap_or(false)
}

/// TCP peer_addr — 获取远端地址
pub fn tcp_peer_addr(socket: &SocketFile) -> Result<[u8; 16], SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        tcp_socket.remote_endpoint()
            .map(endpoint_to_addr)
            .ok_or(SocketError::NotConnected)
    }).unwrap_or(Err(SocketError::NotConnected))
}

// ============================================================================
// UDP 操作 — per Linux net/ipv4/udp.c / inet_dgram_ops
// ============================================================================

/// UDP bind — 绑定本地端口
pub fn udp_bind(socket: &Arc<SocketFile>, addr: [u8; 16], _addrlen: usize) -> Result<(), SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    let ep = addr_to_listen_endpoint(&addr);

    net_stack::with_socket_set(|sockets, _iface| {
        let udp_socket = sockets.get_mut::<smol_udp::Socket>(handle);
        udp_socket.bind(ep).map_err(|_| SocketError::AddrInUse)
    }).unwrap_or(Err(SocketError::NoResources))
}

/// UDP connect — 记录默认目标地址（不实际发送）
pub fn udp_connect(socket: &Arc<SocketFile>, _dest: [u8; 16]) -> Result<(), SocketError> {
    // UDP connect 只是记录 peer address，不发送任何包
    // smoltcp UDP 没有 connect 概念，我们在 socket state 记录
    let handle = socket.with_state(|state| state.backend.smoltcp());
    if handle.is_none() {
        return Err(SocketError::NoResources);
    }

    // 如果未 bind，自动 bind 到临时端口
    let bound = socket.with_state(|state| state.bound);
    if !bound {
        let port = super::socket::allocate_ephemeral_port(49152);
        let mut bind_addr = [0u8; 16];
        bind_addr[0] = 2; // AF_INET
        let port_bytes = port.to_be_bytes();
        bind_addr[2] = port_bytes[0];
        bind_addr[3] = port_bytes[1];
        // bind to 0.0.0.0:port
        udp_bind(socket, bind_addr, 16)?;
        socket.with_state_mut(|state| {
            state.bound_addr = bind_addr;
            state.bound = true;
        });
    }

    Ok(())
}

/// UDP sendto — 发送数据报
pub fn udp_sendto(socket: &Arc<SocketFile>, data: &[u8], dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    let (handle, bound, _bound_addr, peer_addr) = socket.with_state(|state| {
        (state.backend.smoltcp(), state.bound, state.bound_addr,
         state.backend.loopback().peer_addr)
    });
    let handle = handle.ok_or(SocketError::NoResources)?;

    // 确定目标地址
    let dest_ep = if let Some(dest) = dest {
        addr_to_endpoint(&dest)
    } else if let Some(peer) = peer_addr {
        addr_to_endpoint(&peer)
    } else {
        return Err(SocketError::NotConnected);
    };

    // 如果未 bind，自动 bind
    if !bound {
        let port = super::socket::allocate_ephemeral_port(49152);
        let listen_ep = IpListenEndpoint { addr: None, port };
        let bind_result = net_stack::with_socket_set(|sockets, _iface| {
            let udp_socket = sockets.get_mut::<smol_udp::Socket>(handle);
            udp_socket.bind(listen_ep).map_err(|_| SocketError::AddrInUse)
        }).unwrap_or(Err(SocketError::NoResources));
        if let Err(e) = bind_result {
            return Err(e);
        }
        let mut new_addr = [0u8; 16];
        new_addr[0] = 2;
        let port_bytes = port.to_be_bytes();
        new_addr[2] = port_bytes[0];
        new_addr[3] = port_bytes[1];
        socket.with_state_mut(|state| {
            state.bound_addr = new_addr;
            state.bound = true;
        });
    }

    net_stack::poll_net();

    let result = net_stack::with_socket_set(|sockets, _iface| {
        let udp_socket = sockets.get_mut::<smol_udp::Socket>(handle);

        if !udp_socket.can_send() {
            return Err(SocketError::WouldBlock);
        }

        udp_socket.send_slice(data, dest_ep)
            .map(|_| data.len())
            .map_err(|_| SocketError::NoResources)
    }).unwrap_or(Err(SocketError::NoResources));

    if crate::cmdline::netdbg_enabled() {
        crate::println!("[netdbg] udp_sendto dest={:?} len={} result_ok={}",
            dest_ep, data.len(), result.is_ok());
    }

    if result.is_ok() {
        net_stack::poll_net();
    }

    result
}

/// UDP recvfrom — 接收数据报
pub fn udp_recvfrom(socket: &Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    net_stack::poll_net();

    net_stack::with_socket_set(|sockets, _iface| {
        let udp_socket = sockets.get_mut::<smol_udp::Socket>(handle);

        if crate::cmdline::netdbg_enabled() {
            crate::println!("[netdbg] udp_recvfrom can_recv={} endpoint={:?}",
                udp_socket.can_recv(), udp_socket.endpoint());
        }

        if !udp_socket.can_recv() {
            return Err(SocketError::WouldBlock);
        }

        // 堆分配：64KB 放栈上会撑爆 16 页内核栈（见 backend::recvfrom 同因）
        let mut buf = alloc::vec![0u8; 65536];
        match udp_socket.recv_slice(&mut buf) {
            Ok((size, meta)) => {
                let from = endpoint_to_addr(meta.endpoint);
                buf.truncate(size);
                Ok((buf, from))
            }
            Err(_) => Err(SocketError::WouldBlock),
        }
    }).unwrap_or(Err(SocketError::NoResources))
}

/// UDP read_ready — 检查是否有数据可读
pub fn udp_read_ready(socket: &SocketFile) -> bool {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return false };

    net_stack::poll_net_nonblocking();
    net_stack::with_socket_set(|sockets, _iface| {
        let udp_socket = sockets.get_mut::<smol_udp::Socket>(handle);
        udp_socket.can_recv()
    }).unwrap_or(false)
}

/// UDP peer_addr — 获取 connected 目标地址
pub fn udp_peer_addr(socket: &SocketFile) -> Result<[u8; 16], SocketError> {
    socket.with_state(|state| {
        state.backend.loopback().peer_addr.ok_or(SocketError::NotConnected)
    })
}

// ============================================================================
// ICMP — per Linux net/ipv4/icmp.c (ping socket)
// ============================================================================

use smoltcp::socket::icmp as smol_icmp;

/// ICMP bind — 绑定 ICMP ident（类似 Linux ping socket 的 bind 语义）
pub fn icmp_bind(socket: &Arc<SocketFile>, addr: [u8; 16], _addrlen: usize) -> Result<(), SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    // 对于 ICMP ping socket，port 字段用作 ICMP identifier
    let ident = u16::from_be_bytes([addr[2], addr[3]]);
    let endpoint = smol_icmp::Endpoint::Ident(ident);

    net_stack::with_socket_set(|sockets, _iface| {
        let icmp_socket = sockets.get_mut::<smol_icmp::Socket>(handle);
        icmp_socket.bind(endpoint).map_err(|_| SocketError::AddrInUse)
    }).unwrap_or(Err(SocketError::NoResources))
}

/// ICMP sendto — 发送 ICMP echo request
///
/// 对标 Linux ping socket：用户空间提供 ICMP payload（包含 type/code/checksum/id/seq + data），
/// 内核填充 IP 头并发送。smoltcp 的 ICMP socket 接受完整的 ICMP 包（从 type 字段开始）。
pub fn icmp_sendto(socket: &Arc<SocketFile>, data: &[u8], dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    let (handle, bound) = socket.with_state(|state| {
        (state.backend.smoltcp(), state.bound)
    });
    let handle = handle.ok_or(SocketError::NoResources)?;

    // 确定目标 IP
    let dest_addr = if let Some(dest) = dest {
        let ip = Ipv4Address::new(dest[4], dest[5], dest[6], dest[7]);
        IpAddress::Ipv4(ip)
    } else {
        // 如果没有指定目标，使用之前 connect 记录的 peer
        let peer = socket.with_state(|state| state.backend.loopback().peer_addr);
        match peer {
            Some(p) => IpAddress::Ipv4(Ipv4Address::new(p[4], p[5], p[6], p[7])),
            None => return Err(SocketError::NotConnected),
        }
    };

    // 如果未 bind，自动 bind 到一个 ident
    if !bound {
        let ident = super::socket::allocate_ephemeral_port(1000);
        let endpoint = smol_icmp::Endpoint::Ident(ident);
        let _ = net_stack::with_socket_set(|sockets, _iface| {
            let icmp_socket = sockets.get_mut::<smol_icmp::Socket>(handle);
            icmp_socket.bind(endpoint)
        });
        socket.with_state_mut(|state| {
            state.bound = true;
        });
    }

    net_stack::poll_net();

    let result = net_stack::with_socket_set(|sockets, _iface| {
        let icmp_socket = sockets.get_mut::<smol_icmp::Socket>(handle);

        if !icmp_socket.can_send() {
            return Err(SocketError::WouldBlock);
        }

        icmp_socket.send_slice(data, dest_addr)
            .map(|_| data.len())
            .map_err(|_| SocketError::NoResources)
    }).unwrap_or(Err(SocketError::NoResources));

    if result.is_ok() {
        net_stack::poll_net();
    }

    result
}

/// ICMP recvfrom — 接收 ICMP echo reply
pub fn icmp_recvfrom(socket: &Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    net_stack::poll_net();

    net_stack::with_socket_set(|sockets, _iface| {
        let icmp_socket = sockets.get_mut::<smol_icmp::Socket>(handle);

        if !icmp_socket.can_recv() {
            return Err(SocketError::WouldBlock);
        }

        let mut buf = [0u8; 4096];
        match icmp_socket.recv_slice(&mut buf) {
            Ok((size, src_addr)) => {
                let from = match src_addr {
                    IpAddress::Ipv4(ip) => {
                        let mut addr = [0u8; 16];
                        addr[0] = 2; // AF_INET
                        let octets = ip.0;
                        addr[4] = octets[0];
                        addr[5] = octets[1];
                        addr[6] = octets[2];
                        addr[7] = octets[3];
                        addr
                    }
                };
                Ok((buf[..size].to_vec(), from))
            }
            Err(_) => Err(SocketError::WouldBlock),
        }
    }).unwrap_or(Err(SocketError::NoResources))
}

/// ICMP read_ready
pub fn icmp_read_ready(socket: &SocketFile) -> bool {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return false };

    net_stack::poll_net_nonblocking();
    net_stack::with_socket_set(|sockets, _iface| {
        let icmp_socket = sockets.get_mut::<smol_icmp::Socket>(handle);
        icmp_socket.can_recv()
    }).unwrap_or(false)
}

// ============================================================================
// RAW IP — 对标 Linux net/ipv4/raw.c
//
// SOCK_RAW socket（IPPROTO_ICMP 以外的协议）在 smoltcp 里是 raw::Socket，
// 其 send/recv 缓冲区承载完整 IP 包。Linux raw(7) 非 IP_HDRINCL 语义：用户
// 提供 L4 负载，内核负责构造 IP 头；接收时返回含 IP 头的完整包。
//
// 本机网络栈仅配置 IPv4 地址（10.0.2.15），未配置任何 IPv6 地址，因此 IPv6
// raw socket 无可用源地址 → 发送返回 EADDRNOTAVAIL（与无 IPv6 配置的真实
// 主机一致），而不是把 raw handle 误当 UDP socket 取用导致类型不匹配 panic。
// ============================================================================

use smoltcp::socket::raw as smol_raw;
use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv4Repr};
use smoltcp::phy::ChecksumCapabilities;

/// 本机 IPv4 地址（与 net_stack 的 iface 地址一致）
const LOCAL_IPV4: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);

/// RAW sendto — 用户提供 L4 负载，内核构造 IPv4 头后交给 smoltcp raw socket。
pub fn raw_sendto(socket: &Arc<SocketFile>, data: &[u8], dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    let (handle, proto, reported_domain, peer_addr) = socket.with_state(|state| {
        (state.backend.smoltcp(), state.proto, state.reported_domain,
         state.backend.loopback().peer_addr)
    });
    let handle = handle.ok_or(SocketError::NoResources)?;

    // IPv6 raw：网络栈未配置 IPv6 地址，无可用源地址（同真实无 IPv6 主机）。
    if reported_domain == 10 {
        return Err(SocketError::AddrNotAvail);
    }

    let dest = match dest.or(peer_addr) {
        Some(d) => d,
        None => return Err(SocketError::NotConnected),
    };
    let dst = Ipv4Address::new(dest[4], dest[5], dest[6], dest[7]);

    // 构造 IPv4 头（header checksum 由 emit 计算）+ 拼接用户 L4 负载。
    let repr = Ipv4Repr {
        src_addr: LOCAL_IPV4,
        dst_addr: dst,
        next_header: IpProtocol::from(proto as u8),
        payload_len: data.len(),
        hop_limit: 64,
    };
    let header_len = repr.buffer_len();
    let mut packet = alloc::vec![0u8; header_len + data.len()];
    {
        let mut ipv4 = Ipv4Packet::new_unchecked(&mut packet[..]);
        repr.emit(&mut ipv4, &ChecksumCapabilities::default());
    }
    packet[header_len..].copy_from_slice(data);

    net_stack::poll_net();
    let result = net_stack::with_socket_set(|sockets, _iface| {
        let raw_socket = sockets.get_mut::<smol_raw::Socket>(handle);
        if !raw_socket.can_send() {
            return Err(SocketError::WouldBlock);
        }
        raw_socket.send_slice(&packet)
            .map(|_| data.len())
            .map_err(|_| SocketError::NoResources)
    }).unwrap_or(Err(SocketError::NoResources));

    if result.is_ok() {
        net_stack::poll_net();
    }
    result
}

/// RAW recvfrom — 返回含 IPv4 头的完整包（Linux raw(7) IPv4 语义）。
pub fn raw_recvfrom(socket: &Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let handle = handle.ok_or(SocketError::NoResources)?;

    net_stack::poll_net();
    net_stack::with_socket_set(|sockets, _iface| {
        let raw_socket = sockets.get_mut::<smol_raw::Socket>(handle);
        if !raw_socket.can_recv() {
            return Err(SocketError::WouldBlock);
        }
        let mut buf = [0u8; 4096];
        match raw_socket.recv_slice(&mut buf) {
            Ok(size) => {
                let mut from = [0u8; 16];
                from[0] = 2; // AF_INET
                // IPv4 源地址位于头部 offset 12..16
                if size >= 20 {
                    from[4..8].copy_from_slice(&buf[12..16]);
                }
                Ok((buf[..size].to_vec(), from))
            }
            Err(_) => Err(SocketError::WouldBlock),
        }
    }).unwrap_or(Err(SocketError::NoResources))
}

/// RAW read_ready
pub fn raw_read_ready(socket: &SocketFile) -> bool {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return false };

    net_stack::poll_net_nonblocking();
    net_stack::with_socket_set(|sockets, _iface| {
        let raw_socket = sockets.get_mut::<smol_raw::Socket>(handle);
        raw_socket.can_recv()
    }).unwrap_or(false)
}

// ============================================================================
// Sockopt 真实应用 — 将 KernelSocketState 的 option 值写入 smoltcp socket
// ============================================================================

/// 将 TCP_NODELAY 应用到 smoltcp TCP socket
/// smoltcp TCP socket 的 Nagle 算法可以通过 set_nagle_enabled 控制
pub fn apply_tcp_nodelay(socket: &SocketFile, nodelay: bool) {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return };

    let _ = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        tcp_socket.set_nagle_enabled(!nodelay);
    });
}

/// 将 TCP keepalive 参数应用到 smoltcp TCP socket
pub fn apply_tcp_keepalive(socket: &SocketFile, keepalive: bool, idle_secs: i32, interval_secs: i32) {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return };

    let _ = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        if keepalive && idle_secs > 0 {
            let duration = smoltcp::time::Duration::from_secs(idle_secs as u64);
            tcp_socket.set_keep_alive(Some(duration));
        } else {
            tcp_socket.set_keep_alive(None);
        }
        // smoltcp 不直接支持 interval/count，但 keep_alive 超时时间等效 idle
        let _ = interval_secs;
    });
}

/// 将 IP_TTL / hop_limit 应用到 smoltcp TCP socket
pub fn apply_tcp_hop_limit(socket: &SocketFile, ttl: i32) {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return };

    let _ = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        if ttl > 0 && ttl <= 255 {
            tcp_socket.set_hop_limit(Some(ttl as u8));
        }
    });
}

/// 将 SO_RCVBUF/SO_SNDBUF 等缓冲区大小信息同步
/// 注意：smoltcp 的 buffer 大小在创建时确定，运行时无法调整
/// 这里只做尽力同步（将来可通过重建 socket 实现）
pub fn apply_tcp_timeout(socket: &SocketFile, timeout_secs: i64) {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return };

    let _ = net_stack::with_socket_set(|sockets, _iface| {
        let tcp_socket = sockets.get_mut::<smol_tcp::Socket>(handle);
        if timeout_secs > 0 {
            let duration = smoltcp::time::Duration::from_secs(timeout_secs as u64);
            tcp_socket.set_timeout(Some(duration));
        } else {
            tcp_socket.set_timeout(None);
        }
    });
}

/// 将 IP_TTL 应用到 UDP socket 的 hop limit
pub fn apply_udp_hop_limit(socket: &SocketFile, ttl: i32) {
    let handle = socket.with_state(|state| state.backend.smoltcp());
    let Some(handle) = handle else { return };

    let _ = net_stack::with_socket_set(|sockets, _iface| {
        let udp_socket = sockets.get_mut::<smol_udp::Socket>(handle);
        if ttl > 0 && ttl <= 255 {
            udp_socket.set_hop_limit(Some(ttl as u8));
        }
    });
}
