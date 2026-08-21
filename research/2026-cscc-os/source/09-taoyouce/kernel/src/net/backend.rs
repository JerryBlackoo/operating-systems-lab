//! Socket backend 路由层 — 按 domain/type 将操作分发到正确的协议栈
//!
//! 路由逻辑（对标 Linux net/socket.c `sock_create` → proto_ops）：
//! - AF_UNIX (domain=1)  → loopback 内存路径
//! - AF_INET (domain=2)  → smoltcp 真实 TCP/IP 协议栈 (inet.rs)
//! - AF_ALG  (domain=38) → af_alg 加密 socket
//! - AF_NETLINK/AF_PACKET → 各自的子模块

use alloc::sync::Arc;
use alloc::vec::Vec;
use smoltcp::iface::SocketHandle;

use super::af_alg;
use super::inet;
use super::loopback;
use super::net_stack;
use super::socket::{SocketError, SocketFile};

// ============================================================================
// SmoltcpSocketHandle — 区分 TCP/UDP handle
// ============================================================================

pub(in crate::net) enum SmoltcpSocketHandle {
    Tcp(SocketHandle),
    Udp(SocketHandle),
    Icmp(SocketHandle),
    Raw(SocketHandle),
}

impl SmoltcpSocketHandle {
    fn raw(self) -> SocketHandle {
        match self {
            Self::Tcp(handle) | Self::Udp(handle) | Self::Icmp(handle) | Self::Raw(handle) => handle,
        }
    }
}

// ============================================================================
// SocketBackendState — 每个 socket 的后端状态
// ============================================================================

pub(in crate::net) struct SocketBackendState {
    loopback: loopback::LoopbackState,
    smoltcp: Option<SmoltcpSocketHandle>,
    af_alg: af_alg::AfAlgState,
}

impl SocketBackendState {
    pub(in crate::net) fn new(domain: i32, sock_type: i32, proto: i32) -> Self {
        let smoltcp = if domain == 2 {
            match (sock_type, proto) {
                (1, 0 | 6) => net_stack::add_tcp_socket().map(SmoltcpSocketHandle::Tcp),
                (2, 0 | 17) => net_stack::add_udp_socket().map(SmoltcpSocketHandle::Udp),
                // SOCK_DGRAM + IPPROTO_ICMP = ping socket (Linux 3.0+)
                (2, 1) => net_stack::add_icmp_socket().map(SmoltcpSocketHandle::Icmp),
                // SOCK_RAW + IPPROTO_ICMP = raw ICMP socket
                (3, 1) => net_stack::add_icmp_socket().map(SmoltcpSocketHandle::Icmp),
                // SOCK_RAW + other protocol = raw IP socket
                (3, p) => {
                    let ip_proto = smoltcp::wire::IpProtocol::from(p as u8);
                    net_stack::add_raw_socket(smoltcp::wire::IpVersion::Ipv4, ip_proto)
                        .map(SmoltcpSocketHandle::Raw)
                }
                _ => None,
            }
        } else {
            None
        };
        Self {
            loopback: loopback::LoopbackState::new(),
            smoltcp,
            af_alg: af_alg::AfAlgState::new(),
        }
    }

    pub(in crate::net) fn loopback(&self) -> &loopback::LoopbackState {
        &self.loopback
    }

    pub(in crate::net) fn loopback_mut(&mut self) -> &mut loopback::LoopbackState {
        &mut self.loopback
    }

    pub(in crate::net) fn smoltcp(&self) -> Option<SocketHandle> {
        match &self.smoltcp {
            Some(SmoltcpSocketHandle::Tcp(handle))
            | Some(SmoltcpSocketHandle::Udp(handle))
            | Some(SmoltcpSocketHandle::Icmp(handle))
            | Some(SmoltcpSocketHandle::Raw(handle)) => Some(*handle),
            None => None,
        }
    }

    pub(in crate::net) fn is_tcp(&self) -> bool {
        matches!(&self.smoltcp, Some(SmoltcpSocketHandle::Tcp(_)))
    }

    pub(in crate::net) fn is_udp(&self) -> bool {
        matches!(&self.smoltcp, Some(SmoltcpSocketHandle::Udp(_)))
    }

    pub(in crate::net) fn is_icmp(&self) -> bool {
        matches!(&self.smoltcp, Some(SmoltcpSocketHandle::Icmp(_)))
    }

    pub(in crate::net) fn is_raw(&self) -> bool {
        matches!(&self.smoltcp, Some(SmoltcpSocketHandle::Raw(_)))
    }

    /// 交换 smoltcp handle（用于 accept：将已连接的 handle 给新 socket，
    /// listener 获得新分配的 handle）
    pub(in crate::net) fn swap_smoltcp_handle(&mut self, new_handle: SocketHandle) {
        // 保留类型信息（TCP accept 产生的总是 TCP）
        if self.is_tcp() {
            self.smoltcp = Some(SmoltcpSocketHandle::Tcp(new_handle));
        } else if self.is_udp() {
            self.smoltcp = Some(SmoltcpSocketHandle::Udp(new_handle));
        }
    }

    /// 设置 smoltcp handle（用于 accepted socket 获取已连接的 handle）
    pub(in crate::net) fn set_smoltcp_handle(&mut self, handle: SocketHandle) {
        // accepted socket 总是 TCP
        self.smoltcp = Some(SmoltcpSocketHandle::Tcp(handle));
    }

    pub(in crate::net) fn af_alg(&self) -> &af_alg::AfAlgState {
        &self.af_alg
    }

    pub(in crate::net) fn af_alg_mut(&mut self) -> &mut af_alg::AfAlgState {
        &mut self.af_alg
    }

    fn take_smoltcp(&mut self) -> Option<SmoltcpSocketHandle> {
        self.smoltcp.take()
    }
}

// ============================================================================
// 路由判断 — 决定走 inet (smoltcp) 还是 loopback
// ============================================================================

/// AF_INET socket 且有 smoltcp handle → 走真实网络栈
fn use_inet(socket: &SocketFile) -> bool {
    socket.with_state(|state| state.domain == 2 && state.backend.smoltcp().is_some())
}

/// 目标是否为本机地址 — 对标 Linux 路由判定（127/8、INADDR_ANY、本机 iface 地址
/// 均走 lo 本地投递）
fn inet_dest_is_local(dest: &[u8; 16]) -> bool {
    let ip = [dest[4], dest[5], dest[6], dest[7]];
    ip[0] == 127 || ip == [0, 0, 0, 0] || net_stack::is_local_ip(ip)
}

/// UDP 发送路由 — 对标 Linux udp_sendmsg：目标为本机地址时经 lo 本地投递
/// （未绑定的发送方先自动绑定临时端口，对应 inet_autobind），
/// 否则交给 smoltcp 真实网络栈。
fn udp_send_routed(socket: Arc<SocketFile>, data: &[u8], dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    udp_send_routed_owned(socket, data.to_vec(), dest)
}

fn udp_send_routed_owned(socket: Arc<SocketFile>, data: Vec<u8>, dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    let resolved_dest = dest.or_else(|| socket.with_state(|state| state.backend.loopback().peer_addr));
    if resolved_dest.is_none() {
        crate::println!("[be] udp_send_routed: sid={} NO DEST!", socket.socket_id());
    }
    if let Some(d) = resolved_dest {
        if inet_dest_is_local(&d) {
            socket.with_state_mut(|state| {
                if !state.bound {
                    let port = super::socket::allocate_ephemeral_port(49152);
                    state.bound_addr[0] = 2;
                    state.bound_addr[2] = (port >> 8) as u8;
                    state.bound_addr[3] = (port & 0xff) as u8;
                    state.bound = true;
                }
            });
            // Use udp_sendto which correctly filters receivers by
            // sock_type==SOCK_DGRAM, avoiding misdelivery to a TCP
            // listener sharing the same port number.
            return super::udp::udp_sendto_owned(&socket, data, Some(d), 0);
        }
    }
    inet::udp_sendto(&socket, &data, dest)
}

/// TCP 发送路由 — 本地（lo）连接的 socket 有 loopback peer_id，
/// 数据直接入对端接收队列；否则走 smoltcp 真实网络栈。
fn tcp_send_routed(socket: Arc<SocketFile>, data: &[u8]) -> Result<usize, SocketError> {
    if socket.with_state(|state| state.backend.loopback().peer_id.is_some()) {
        return loopback::sendto(socket, data, None);
    }
    inet::tcp_send(&socket, data)
}

// ============================================================================
// 公共 backend API — 按 domain 路由
// ============================================================================

pub(in crate::net) fn release(socket: &SocketFile) {
    let sid = socket.socket_id();
    // Closing the fd tears down our write half too — deliver EOF to a connected
    // loopback peer so its blocked recv returns 0. No-op without a loopback peer.
    loopback::notify_peer_closed(socket);
    let is_tcp = socket.with_state(|state| state.backend.is_tcp());
    if let Some(handle) = socket.with_state_mut(|state| state.backend.take_smoltcp()) {
        let raw_handle = handle.raw();
        // TCP: 先发送 FIN（graceful close）再移除
        if is_tcp {
            let _ = net_stack::with_socket_set(|sockets, _iface| {
                let tcp_socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(raw_handle);
                tcp_socket.close();
            });
            // 给一次 poll 机会发送 FIN
            net_stack::poll_net();
        }
        net_stack::remove_socket(raw_handle);
    }
}

pub fn bind(socket: Arc<SocketFile>, addr: [u8; 16], addrlen: usize) -> Result<(), SocketError> {
    let (domain, reported) = socket.with_state(|state| (state.domain, state.reported_domain));
    // AF_PACKET (17) is downgraded to AF_INET for backend routing, but bind()
    // takes a struct sockaddr_ll (not sockaddr_in). Route it to the packet
    // socket bind, which records the bound interface/protocol — per Linux
    // net/packet/af_packet.c packet_bind(). Without this the sockaddr_ll is
    // misread as a sockaddr_in and rejected with EADDRNOTAVAIL.
    if reported == 17 {
        return super::af_packet::packet_bind(&socket, &addr, addrlen);
    }
    match domain {
        38 => af_alg::bind(socket, &addr, addrlen),
        2 => {
            // AF_INET: 先做地址验证（复用 loopback 的检查逻辑），
            // 然后如果有 smoltcp handle，也通知 smoltcp
            let (is_udp, is_icmp) = socket.with_state(|state| {
                (state.backend.is_udp(), state.backend.is_icmp())
            });
            // 基础验证 + 记录 bound_addr
            loopback::bind(socket.clone(), addr, addrlen)?;
            // loopback::bind 已把 port 0 解析为分配的临时端口（写入
            // bound_addr），smoltcp bind 必须用解析后的地址 — smoltcp 对
            // port 0 直接报错，会让 socket 停留在未绑定状态。
            let resolved_addr = socket.with_state(|state| state.bound_addr);
            if is_udp {
                // UDP: smoltcp 需要立即 bind
                inet::udp_bind(&socket, resolved_addr, addrlen).or_else(|_| Ok(()))?;
            } else if is_icmp {
                // ICMP: bind ident
                inet::icmp_bind(&socket, resolved_addr, addrlen).or_else(|_| Ok(()))?;
            }
            // TCP: bind 只记录地址，在 listen/connect 时才真正绑定到 smoltcp
            Ok(())
        }
        _ => loopback::bind(socket, addr, addrlen),
    }
}

pub fn bind_raw(socket: Arc<SocketFile>, addr: &[u8], addrlen: usize) -> Result<(), SocketError> {
    let domain = socket.with_state(|state| state.domain);
    if domain == 38 {
        return af_alg::bind(socket, addr, addrlen);
    }
    let mut short = [0u8; 16];
    let copy_len = addr.len().min(16);
    short[..copy_len].copy_from_slice(&addr[..copy_len]);
    bind(socket, short, addrlen)
}

pub fn listen(socket: Arc<SocketFile>, backlog: i32) -> Result<(), SocketError> {
    if use_inet(&socket) {
        let is_tcp = socket.with_state(|state| state.backend.is_tcp());
        if is_tcp {
            // 先设置 listening 状态
            socket.with_state_mut(|state| {
                if state.sock_type != 1 && state.sock_type != 5 {
                    return Err(SocketError::OpNotSupported);
                }
                state.listening = true;
                Ok(())
            })?;
            // 对 smoltcp TCP socket 执行 listen
            return inet::tcp_listen(&socket);
        }
    }
    loopback::listen(socket, backlog)
}

pub fn accept(socket: Arc<SocketFile>) -> Result<(Arc<SocketFile>, [u8; 16]), SocketError> {
    let domain = socket.with_state(|state| state.domain);
    match domain {
        38 => af_alg::accept(socket),
        2 if socket.with_state(|state| state.backend.is_tcp() && state.backend.smoltcp().is_some()) => {
            // 本地（lo）发起的连接排在 loopback accept_queue，外部连接在
            // smoltcp — 二者汇入同一 listener（对标 Linux 单一 accept 队列）
            match loopback::accept(socket.clone()) {
                Err(SocketError::WouldBlock) => inet::tcp_accept(&socket),
                other => other,
            }
        }
        _ => loopback::accept(socket),
    }
}

pub fn connect(socket: Arc<SocketFile>, dest: [u8; 16]) -> Result<(), SocketError> {
    if use_inet(&socket) {
        let (is_tcp, is_icmp, is_raw) = socket.with_state(|state| {
            (state.backend.is_tcp(), state.backend.is_icmp(), state.backend.is_raw())
        });
        if is_tcp {
            // 目标为本机地址 → 经 lo 本地建连（对标 Linux 本地 TCP 连接）
            if inet_dest_is_local(&dest) {
                return loopback::connect(socket, dest);
            }
            let result = inet::tcp_connect(&socket, dest);
            if result.is_ok() {
                socket.with_state_mut(|state| {
                    state.backend.loopback_mut().peer_addr = Some(dest);
                });
            }
            return result;
        } else if is_icmp || is_raw {
            // ICMP / RAW connect — 只记录目标地址（对标 Linux 无连接 socket）
            socket.with_state_mut(|state| {
                state.backend.loopback_mut().peer_addr = Some(dest);
            });
            return Ok(());
        } else {
            // UDP connect — 记录默认目标
            let result = inet::udp_connect(&socket, dest);
            if result.is_ok() {
                socket.with_state_mut(|state| {
                    state.backend.loopback_mut().peer_addr = Some(dest);
                });
            }
            return result;
        }
    }
    loopback::connect(socket, dest)
}

pub fn sendto(socket: Arc<SocketFile>, data: &[u8], dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    if crate::RTDIAG {
        let t0 = crate::arch::asm_helpers::rdtime();
        let owned = data.to_vec();
        let t1 = crate::arch::asm_helpers::rdtime();
        let r = sendto_owned(socket, owned, dest).map_err(|(e, _)| e);
        let t2 = crate::arch::asm_helpers::rdtime();
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let vec_us = t1.saturating_sub(t0) * 1_000_000 / freq;
        let send_us = t2.saturating_sub(t1) * 1_000_000 / freq;
        if vec_us > 300 || send_us > 300 {
            crate::rtdiag_println!("[rtdiag] bk-sendto vec_us={} send_us={} len={}", vec_us, send_us, data.len());
        }
        return r;
    }
    sendto_owned(socket, data.to_vec(), dest).map_err(|(e, _)| e)
}

/// Ownership-taking send: moves the payload into the receiver's queue on the
/// loopback fast paths (no per-packet copy). On `WouldBlock` the buffer is
/// handed back so blocking callers can retry without re-reading userspace.
pub fn sendto_owned(
    socket: Arc<SocketFile>,
    data: Vec<u8>,
    dest: Option<[u8; 16]>,
) -> Result<usize, (SocketError, Vec<u8>)> {
    let (domain, has_smoltcp, is_tcp, is_icmp, is_raw, has_lo_peer) = socket.with_state(|state| {
        (
            state.domain,
            state.backend.smoltcp().is_some(),
            state.backend.is_tcp(),
            state.backend.is_icmp(),
            state.backend.is_raw(),
            state.backend.loopback().peer_id.is_some(),
        )
    });
    match domain {
        38 => {
            if dest.is_some() {
                return Err((SocketError::OpNotSupported, data));
            }
            let r = af_alg::send(socket, &data, 0, None);
            r.map_err(|e| (e, data))
        }
        2 if has_smoltcp => {
            if is_tcp {
                if has_lo_peer {
                    return loopback::sendto_owned(socket, data, None);
                }
                let r = inet::tcp_send(&socket, &data);
                r.map_err(|e| (e, data))
            } else if is_icmp {
                let r = inet::icmp_sendto(&socket, &data, dest);
                r.map_err(|e| (e, data))
            } else if is_raw {
                let r = inet::raw_sendto(&socket, &data, dest);
                r.map_err(|e| (e, data))
            } else {
                udp_send_routed_owned(socket, data, dest).map_err(|e| (e, Vec::new()))
            }
        }
        _ => loopback::sendto_owned(socket, data, dest),
    }
}

pub fn sendmsg(socket: Arc<SocketFile>, data: &[u8], flags: i32, control: Option<&[u8]>) -> Result<usize, SocketError> {
    let domain = socket.with_state(|state| state.domain);
    match domain {
        38 => af_alg::send(socket, data, flags, control),
        2 if socket.with_state(|state| state.backend.smoltcp().is_some()) => {
            let (is_tcp, is_icmp, is_raw) = socket.with_state(|state| {
                (state.backend.is_tcp(), state.backend.is_icmp(), state.backend.is_raw())
            });
            if is_tcp {
                tcp_send_routed(socket, data)
            } else if is_icmp {
                inet::icmp_sendto(&socket, data, None)
            } else if is_raw {
                inet::raw_sendto(&socket, data, None)
            } else {
                udp_send_routed(socket, data, None)
            }
        }
        1 => {
            // AF_UNIX: SCM_RIGHTS fd resolution is done by the syscall layer
            // which calls sendmsg_unix_with_file_descs directly. If we get here
            // without pre-resolved file descriptions, send with empty fds.
            loopback::sendmsg_with_fds(socket, data, Vec::new())
        }
        _ => loopback::sendto(socket, data, None),
    }
}

/// Receive data from AF_UNIX with ancillary file description passing support.
pub fn recvmsg_unix(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16], Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>), SocketError> {
    loopback::recvmsg_with_fds(socket)
}

/// Receive data from AF_UNIX with file descriptions + credentials.
pub fn recvmsg_unix_creds(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16], Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>, Option<loopback::Ucred>), SocketError> {
    loopback::recvmsg_with_fds_creds(socket)
}

/// Send data on AF_UNIX with pre-resolved SCM_RIGHTS file descriptions.
/// Called from syscall layer after resolving sender fd numbers to Arc<FileDescriptor>.
pub fn sendmsg_unix_with_file_descs(socket: Arc<SocketFile>, data: &[u8], file_descs: Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>) -> Result<usize, SocketError> {
    loopback::sendmsg_with_fds(socket, data, file_descs)
}

/// Parse SCM_RIGHTS from cmsg control buffer (public for syscall layer).
pub fn parse_scm_rights_pub(control: Option<&[u8]>) -> Vec<i32> {
    parse_scm_rights(control)
}

/// Parse SCM_RIGHTS from cmsg control buffer.
/// Linux cmsg layout: struct cmsghdr { cmsg_len, cmsg_level, cmsg_type } + data
/// cmsg_len: size_t (8 bytes on 64-bit), cmsg_level: int (4), cmsg_type: int (4) = 16 byte header
/// SCM_RIGHTS: level=SOL_SOCKET(1), type=SCM_RIGHTS(1), data = array of int (4 bytes each)
fn parse_scm_rights(control: Option<&[u8]>) -> Vec<i32> {
    const SOL_SOCKET: i32 = 1;
    const SCM_RIGHTS: i32 = 1;
    const CMSG_HDR_SIZE: usize = 16; // sizeof(struct cmsghdr) on 64-bit

    let control = match control {
        Some(c) if c.len() >= CMSG_HDR_SIZE => c,
        _ => return Vec::new(),
    };

    let mut fds = Vec::new();
    let mut offset = 0;
    while offset + CMSG_HDR_SIZE <= control.len() {
        let cmsg_len = usize::from_le_bytes(control[offset..offset + 8].try_into().unwrap_or([0; 8]));
        let cmsg_level = i32::from_le_bytes(control[offset + 8..offset + 12].try_into().unwrap_or([0; 4]));
        let cmsg_type = i32::from_le_bytes(control[offset + 12..offset + 16].try_into().unwrap_or([0; 4]));

        if cmsg_len < CMSG_HDR_SIZE { break; }
        let data_len = cmsg_len - CMSG_HDR_SIZE;
        let data_start = offset + CMSG_HDR_SIZE;
        let data_end = (data_start + data_len).min(control.len());

        if cmsg_level == SOL_SOCKET && cmsg_type == SCM_RIGHTS {
            // Each fd is 4 bytes (int)
            let mut fd_offset = data_start;
            while fd_offset + 4 <= data_end {
                let fd = i32::from_le_bytes(control[fd_offset..fd_offset + 4].try_into().unwrap_or([0; 4]));
                fds.push(fd);
                fd_offset += 4;
            }
        }

        // Advance to next cmsg (aligned to 8 bytes)
        let aligned_len = (cmsg_len + 7) & !7;
        offset += aligned_len;
    }
    fds
}

pub fn recvfrom(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    let (domain, has_smoltcp, is_tcp, is_icmp, is_raw, has_lo_peer) = socket.with_state(|state| {
        (
            state.domain,
            state.backend.smoltcp().is_some(),
            state.backend.is_tcp(),
            state.backend.is_icmp(),
            state.backend.is_raw(),
            state.backend.loopback().peer_id.is_some(),
        )
    });
    match domain {
        38 => af_alg::recv(socket),
        2 if has_smoltcp => {
            if is_tcp {
                // 本地（lo）连接的数据在 loopback 队列
                if has_lo_peer {
                    return loopback::recvfrom(socket);
                }
                // 堆分配 64KB 接收缓冲：若放栈上，recvfrom 序言会预留 64KB，
                // 恰好等于内核栈大小（16 页），导致栈溢出污染相邻物理页。
                let mut buf = alloc::vec![0u8; 65536];
                let n = inet::tcp_recv(&socket, &mut buf)?;
                // n == 0 表示 EOF（对端发了 FIN）— 对标 Linux read() 返回 0
                let peer = inet::tcp_peer_addr(&socket).unwrap_or([0u8; 16]);
                buf.truncate(n);
                Ok((buf, peer))
            } else if is_icmp {
                inet::icmp_recvfrom(&socket)
            } else if is_raw {
                // 内核→用户态本地投递（如 AF_NETLINK 降级 raw 套接字承载
                // POSIX mq SIGEV_THREAD 通知 cookie）进入 loopback 队列；
                // 外部到达的原始包在 smoltcp。二者汇入同一接收语义。
                match loopback::recvfrom(socket.clone()) {
                    Err(SocketError::WouldBlock) => inet::raw_recvfrom(&socket),
                    other => other,
                }
            } else {
                // 本地投递的数据报在 loopback 队列，外部到达的在 smoltcp —
                // 二者汇入同一 socket 的接收语义（对标 Linux 单一 receive queue）。
                // 绑定在 127/8 的 socket 收不到外部包（Linux 对 loopback 地址
                // 不做外部路由），跳过 smoltcp 全栈 poll。
                match loopback::recvfrom(socket.clone()) {
                    Err(SocketError::WouldBlock) if !bound_loopback_only(&socket) => {
                        inet::udp_recvfrom(&socket)
                    }
                    other => other,
                }
            }
        }
        _ => loopback::recvfrom(socket),
    }
}

/// Buffer-size-aware recv. For a connected loopback TCP socket (byte stream)
/// this coalesces queued sender writes up to `max_len` and preserves any
/// partially-consumed tail (Linux `tcp_recvmsg` semantics); for every other
/// socket kind it is identical to [`recvfrom`] (datagram/seqpacket message
/// boundaries and the smoltcp/AF_UNIX-SCM paths are unchanged). `max_len` is the
/// caller's user-buffer length.
pub fn recvfrom_coalesce(
    socket: Arc<SocketFile>,
    max_len: usize,
) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    // Only AF_INET TCP with a live loopback peer is a coalescable byte stream.
    // UDP keeps per-datagram boundaries; AF_UNIX stream keeps per-message
    // boundaries because each segment may carry its own SCM_RIGHTS/creds queue
    // entry that must stay in lock-step with recv_buf.
    let is_lo_tcp = socket.with_state(|state| {
        state.domain == 2
            && state.backend.smoltcp().is_some()
            && state.backend.is_tcp()
            && state.backend.loopback().peer_id.is_some()
    });
    if is_lo_tcp {
        return loopback::recvfrom_stream(socket, max_len);
    }
    recvfrom(socket)
}

pub fn peer_addr(socket: &SocketFile) -> Result<[u8; 16], SocketError> {
    let domain = socket.with_state(|state| state.domain);
    if domain == 2 {
        let is_tcp = socket.with_state(|state| state.backend.is_tcp());
        if is_tcp {
            // 优先从 smoltcp 获取真实 remote endpoint
            if let Ok(addr) = inet::tcp_peer_addr(socket) {
                return Ok(addr);
            }
        }
    }
    // fallback 到 loopback 记录的 peer addr
    loopback::peer_addr(socket)
}

pub fn shutdown(socket: Arc<SocketFile>, how: i32) -> Result<(), SocketError> {
    if use_inet(&socket) {
        let is_tcp = socket.with_state(|state| state.backend.is_tcp());
        if is_tcp {
            inet::tcp_shutdown(&socket, how)?;
        }
    }
    // SHUT_WR(1)/SHUT_RDWR(2): deliver EOF to a connected loopback peer so its
    // blocked recv returns 0 instead of hanging. No-op without a loopback peer.
    if how != 0 {
        loopback::notify_peer_closed(&socket);
        // Our write half is gone: future sends fail EPIPE, but the read half
        // stays fully usable (Linux only sends a FIN), so the socket must NOT
        // be marked closed for SHUT_WR alone.
        socket.with_state_mut(|state| state.snd_shutdown = true);
        socket.wake_writers();
    }
    // SHUT_RD(0)/SHUT_RDWR(2): mark the read half hung up so epoll surfaces
    // EPOLLRDHUP for a watcher on this socket (Linux delivers EPOLLRDHUP on
    // local read-shutdown as well as peer write-close).
    if how == 0 || how == 2 {
        socket.with_state_mut(|state| state.rcv_shutdown = true);
        socket.wake_readers();
    }
    // 全关（SHUT_RDWR）才标记 socket 关闭状态（唤醒阻塞的 reader/writer）
    if how == 2 {
        socket.mark_closed();
    }
    Ok(())
}

pub fn read_ready(socket: &SocketFile) -> bool {
    let domain = socket.with_state(|state| state.domain);
    match domain {
        38 => af_alg::read_ready(socket),
        2 if socket.with_state(|state| state.backend.smoltcp().is_some()) => {
            let (is_tcp, is_icmp, is_raw, is_listening, has_lb_peer) = socket.with_state(|state| {
                (state.backend.is_tcp(), state.backend.is_icmp(), state.backend.is_raw(),
                 state.listening, state.backend.loopback().peer_id.is_some())
            });
            if is_tcp {
                // A listening TCP socket's readability means "an incoming
                // connection is pending to accept", NOT recv-buffer state.
                // Consult accept-readiness only: the loopback accept queue
                // (loopback connections) or smoltcp's established-connection
                // signal (external connections). Routing a listener through the
                // recv-path tcp_read_ready() is wrong — that returns true for a
                // dormant LISTEN handle (!is_active()), so register_waiter()
                // sees the socket as already readable, skips enqueuing the
                // accept waiter, and the wakeup delivered by connect() is lost.
                if is_listening {
                    return loopback::read_ready(socket) || inet::tcp_accept_ready(socket);
                }
                // A loopback-connected TCP socket carries its data in the
                // loopback recv queue; its smoltcp handle stays dormant and
                // would falsely report ready (its !is_active() == true). Mirror
                // recvfrom's routing and consult only the loopback queue.
                if has_lb_peer {
                    return loopback::read_ready(socket);
                }
                loopback::read_ready(socket) || inet::tcp_read_ready(socket)
            } else if is_icmp {
                inet::icmp_read_ready(socket)
            } else if is_raw {
                inet::raw_read_ready(socket)
            } else {
                loopback::read_ready(socket)
                    || (!bound_loopback_only(socket) && inet::udp_read_ready(socket))
            }
        }
        _ => loopback::read_ready(socket),
    }
}

/// 该 socket 只可能收到本地投递的包：要么显式绑在 127/8（外部包不会
/// 路由到 loopback 地址），要么已 connect 到 127/8 peer（Linux connected
/// UDP 只接受来自该 peer 的包，而 loopback peer 只能本地投递）。
/// 此时接收侧无需咨询 smoltcp（poll 整个网络栈的开销可观）。
fn bound_loopback_only(socket: &SocketFile) -> bool {
    socket.with_state(|state| {
        (state.bound && state.bound_addr[4] == 127)
            || state
                .backend
                .loopback()
                .peer_addr
                .map(|p| p[4] == 127)
                .unwrap_or(false)
    })
}

pub fn write_ready(socket: &SocketFile) -> bool {
    let (closed, domain, has_loopback_peer, has_smoltcp, is_tcp) = socket.with_state(|state| {
        (
            state.closed,
            state.domain,
            state.backend.loopback().peer_id.is_some(),
            state.backend.smoltcp().is_some(),
            state.backend.is_tcp(),
        )
    });
    if closed {
        return false;
    }
    if domain == 2 && has_smoltcp && is_tcp && !has_loopback_peer {
        return true;
    }
    loopback::write_ready(socket)
}
