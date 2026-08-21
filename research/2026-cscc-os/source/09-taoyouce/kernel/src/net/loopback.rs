use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use super::socket::{create_socket_file, all_socket_files, allocate_ephemeral_port, family_of, normalize_bind_addr, port_of, register_unix_pathname, socket_by_id, SocketError, SocketFile};

/// Per-message ancillary credentials (SCM_CREDENTIALS / struct ucred).
#[derive(Clone, Copy, Debug, Default)]
pub struct Ucred {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

pub(in crate::net) struct LoopbackState {
    pub(in crate::net) recv_buf: alloc::collections::VecDeque<(Vec<u8>, [u8; 16])>,
    pub(in crate::net) peer_id: Option<usize>,
    /// Cached strong-ref shortcut for `peer_id`: avoids the global
    /// SOCKET_FILES lock + BTreeMap lookup on every data-path packet.
    /// `peer_id` stays the source of truth; the cache is validated against
    /// it on use and lazily (re)populated. Weak, so an unregistered peer
    /// can still be freed (no Arc cycle between connected endpoints).
    pub(in crate::net) peer_cache: Option<Weak<SocketFile>>,
    /// UDP unicast demux cache: (dest_port, sender_port, receiver). Only ever
    /// holds a pass-1 (connected, peer==sender) match from `find_udp_receiver`
    /// and is revalidated against live receiver state before each use, so it
    /// is a pure scan-skip — never changes which socket a datagram reaches.
    pub(in crate::net) udp_dest_cache: Option<(u16, u16, Weak<SocketFile>)>,
    pub(in crate::net) peer_addr: Option<[u8; 16]>,
    pub(in crate::net) accept_queue: alloc::collections::VecDeque<usize>,
    /// SCM_RIGHTS fd passing queue: each entry is a list of Arc<FileDescriptor>
    /// objects cloned from the sender's fd table — per Linux semantics, SCM_RIGHTS
    /// passes the underlying struct file *, not just fd numbers.
    pub(in crate::net) pending_fds: alloc::collections::VecDeque<Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>>,
    /// SCM_CREDENTIALS queue: sender ucred per message. Dequeued on recvmsg
    /// when SO_PASSCRED is set on the receiving socket.
    pub(in crate::net) pending_creds: alloc::collections::VecDeque<Ucred>,
    /// Set when the connected peer has closed or shut down its write side.
    /// Mirrors Linux: once the peer's write half is gone, our read half reaches
    /// EOF after the receive buffer drains (recv returns 0, poll reports
    /// readable). Without this a loopback recv would block forever instead of
    /// observing the peer-close, hanging connection-oriented clients.
    pub(in crate::net) peer_closed: bool,
    /// UDP 发送背压的放弃时刻（rdtime 绝对值）：接收队列满时发送方挂起等排空，
    /// 该 sendto 被重启后仍满且超过此时刻则按 Linux 语义丢包。挂在发送方 socket
    /// 上以跨 syscall 重启保留状态（suspend 后 ecall 会整体重放）。
    pub(in crate::net) udp_bp_giveup: Option<u64>,
    /// recv_buf 中排队的总字节数（O(1) 维护）。发送方流控每次 send 都要查
    /// 队列水位；hackbench 100B×80k 段积压下按段遍历求和是 ms 级 O(n) 热点
    /// （单个 write/read syscall 拖长至 2~6ms，堵死 RT 唤醒检测窗口）。
    /// 只能经下方 recv_* helper 增删 recv_buf，保证计数与队列同步。
    pub(in crate::net) recv_bytes: usize,
}

impl LoopbackState {
    pub(in crate::net) fn new() -> Self {
        Self {
            recv_buf: alloc::collections::VecDeque::new(),
            peer_id: None,
            peer_cache: None,
            udp_dest_cache: None,
            peer_addr: None,
            accept_queue: alloc::collections::VecDeque::new(),
            pending_fds: alloc::collections::VecDeque::new(),
            udp_bp_giveup: None,
            pending_creds: alloc::collections::VecDeque::new(),
            peer_closed: false,
            recv_bytes: 0,
        }
    }

    #[inline]
    pub(in crate::net) fn recv_push_back(&mut self, data: Vec<u8>, from: [u8; 16]) {
        self.recv_bytes += data.len();
        self.recv_buf.push_back((data, from));
    }

    #[inline]
    pub(in crate::net) fn recv_push_front(&mut self, data: Vec<u8>, from: [u8; 16]) {
        self.recv_bytes += data.len();
        self.recv_buf.push_front((data, from));
    }

    #[inline]
    pub(in crate::net) fn recv_pop_front(&mut self) -> Option<(Vec<u8>, [u8; 16])> {
        let popped = self.recv_buf.pop_front();
        if let Some((data, _)) = &popped {
            self.recv_bytes -= data.len();
        }
        popped
    }

    #[inline]
    pub(in crate::net) fn recv_clear(&mut self) {
        self.recv_buf.clear();
        self.recv_bytes = 0;
    }
}

fn recv_queue_bytes(state: &LoopbackState) -> usize {
    state.recv_bytes
}

/// Resolve a connected peer through the per-socket cache, falling back to the
/// registry (and refreshing the cache) on miss/stale entry.
pub(in crate::net) fn resolve_peer(
    socket: &SocketFile,
    peer_id: usize,
    cached: Option<Weak<SocketFile>>,
) -> Option<Arc<SocketFile>> {
    if let Some(peer) = cached.and_then(|weak| weak.upgrade()) {
        if peer.socket_id() == peer_id {
            return Some(peer);
        }
    }
    let peer = socket_by_id(peer_id)?;
    let weak = Arc::downgrade(&peer);
    socket.with_state_mut(|state| state.backend.loopback_mut().peer_cache = Some(weak));
    Some(peer)
}

/// Fetch this socket's connected loopback peer (cache-first).
pub(in crate::net) fn peer_socket(socket: &SocketFile) -> Option<Arc<SocketFile>> {
    let (peer_id, cached) = socket.with_state(|state| {
        let lb = state.backend.loopback();
        (lb.peer_id, lb.peer_cache.clone())
    });
    resolve_peer(socket, peer_id?, cached)
}

pub fn bind(socket: Arc<SocketFile>, addr: [u8; 16], addrlen: usize) -> Result<(), SocketError> {
    let (domain, already_bound, reported_domain) = socket.with_state(|state| (state.domain, state.bound, state.reported_domain));
    if already_bound {
        return Err(SocketError::InvalidArg);
    }
    let family = family_of(&addr);
    // Accept AF_INET6 (10) on sockets created as AF_INET6 (internally mapped to AF_INET)
    // Also accept AF_INET (2) on AF_INET sockets, and AF_UNIX (1) on AF_UNIX sockets
    // AF_PACKET (17) and AF_NETLINK (16) similarly mapped
    let family_ok = family == domain
        || (family == reported_domain && domain == 2)  // AF_INET6/PACKET/NETLINK → AF_INET
        || family == 0; // AF_UNSPEC
    if !family_ok {
        return Err(SocketError::AddrFamilyNotSupported);
    }
    if domain == 2 {
        // AF_INET: sockaddr_in = 16 bytes; AF_INET6: sockaddr_in6 = 28 bytes
        let min_len = if family == 10 { 4 } else { 16 };
        if addrlen < min_len {
            return Err(SocketError::InvalidArg);
        }
        // For AF_INET6 binding to :: (any), treat as 0.0.0.0 — always local
        if family != 10 && !is_local_inet_addr(&addr) {
            return Err(SocketError::AddrNotAvail);
        }
    } else if domain == 1 {
        if addrlen < 3 {
            return Err(SocketError::InvalidArg);
        }
        if is_unix_path_current_dir(&addr, addrlen) {
            return Err(SocketError::AddrFamilyNotSupported);
        }
        if unix_path_has_file_prefix(&addr, addrlen) {
            return Err(SocketError::NotDir);
        }
        if find_bound_unix(&addr, addrlen, socket.socket_id()).is_some() {
            return Err(SocketError::AddrInUse);
        }
    }
    socket.with_state_mut(|state| {
        state.bound_addr = normalize_bind_addr(addr);
        state.bound = true;
    });
    if domain == 1 {
        register_unix_pathname(&addr);
    }
    Ok(())
}

pub fn listen(socket: Arc<SocketFile>, _backlog: i32) -> Result<(), SocketError> {
    socket.with_state_mut(|state| {
        if state.closed {
            return Err(SocketError::BadFd);
        }
        if state.sock_type != 1 && state.sock_type != 5 {
            return Err(SocketError::OpNotSupported);
        }
        state.listening = true;
        Ok(())
    })
}

pub fn accept(listener: Arc<SocketFile>) -> Result<(Arc<SocketFile>, [u8; 16]), SocketError> {
    let server_id = listener.with_state_mut(|state| {
        if state.sock_type != 1 && state.sock_type != 5 {
            return Err(SocketError::OpNotSupported);
        }
        if !state.listening || state.closed {
            return Err(SocketError::InvalidArg);
        }
        state.backend.loopback_mut().accept_queue.pop_front().ok_or(SocketError::WouldBlock)
    })?;
    if crate::net::socket::net_trace_enabled() { crate::println!("[TRACE] accept listener_sid={} popped server_sid={}", listener.socket_id(), server_id); } // TRACE
    // The server-side endpoint was created and fully cross-linked at the peer's
    // connect() time (Linux completes the connection independent of accept()).
    // accept() only dequeues this established endpoint, transfers ownership to
    // the accepting process, and reports the connected peer address. Any data
    // the client sent before accept() is already buffered in the endpoint.
    let server = socket_by_id(server_id).ok_or(SocketError::ConnectionRefused)?;
    let client_addr = server.with_state(|state| {
        state.backend.loopback().peer_addr.unwrap_or(state.bound_addr)
    });
    // The endpoint was allocated in the connecting process's context; reassign
    // ownership to the accepting (server) process so process accounting and the
    // client's SO_PEERCRED observe the correct owner.
    let server_cred = current_ucred();
    server.with_state_mut(|state| {
        state.owner_pid = server_cred.pid as usize;
    });
    let client = server.with_state(|state| state.backend.loopback().peer_id).and_then(socket_by_id);
    if let Some(client) = client {
        client.with_state_mut(|state| {
            state.peer_pid = Some(server_cred.pid as usize);
            state.peer_uid = server_cred.uid;
            state.peer_gid = server_cred.gid;
        });
    }
    Ok((server, client_addr))
}

pub fn connect(socket: Arc<SocketFile>, dest: [u8; 16]) -> Result<(), SocketError> {
    let (domain, sock_type) = socket.with_state(|state| (state.domain, state.sock_type));
    let family = u16::from_ne_bytes([dest[0], dest[1]]);
    // AF_UNSPEC (family=0) → disconnect. Per Linux this is valid for both
    // SOCK_DGRAM (clear peer) and SOCK_STREAM (tcp_disconnect): a stream
    // socket is reset to an unconnected, unbound, non-listening state so it
    // can be re-bound and re-listened (connect02 / CVE-2018-9568).
    if family == 0 {
        socket.with_state_mut(|state| {
            state.peer_pid = None;
            state.snd_shutdown = false;
            state.rcv_shutdown = false;
            let lb = state.backend.loopback_mut();
            lb.peer_id = None;
            lb.peer_cache = None;
            lb.peer_addr = None;
            lb.peer_closed = false;
            lb.recv_clear();
            lb.accept_queue.clear();
            if sock_type != 2 {
                // STREAM disconnect releases the bound port and listen state.
                state.bound = false;
                state.bound_addr = [0u8; 16];
                state.listening = false;
            }
        });
        return Ok(());
    }
    // Per Linux: SOCK_DGRAM allows re-connect (change peer). An already
    // connected SOCK_STREAM/SEQPACKET returns EISCONN.
    if sock_type != 2 {
        if socket.with_state(|state| state.backend.loopback().peer_id.is_some() || state.backend.loopback().peer_addr.is_some()) {
            return Err(SocketError::AlreadyConnected);
        }
    }
    let listener = if domain == 1 {
        if sock_type == 2 {
            find_bound_unix(&dest, 16, socket.socket_id())
        } else {
            find_listener_unix(&dest, 16, socket.socket_id())
        }
    } else {
        if sock_type == 2 {
            find_bound_receiver(port_of(&dest), socket.socket_id())
        } else {
            find_listener(port_of(&dest), socket.socket_id())
        }
    }.ok_or(SocketError::ConnectionRefused)?;
    let client_addr = socket.with_state_mut(|state| {
        if !state.bound {
            if domain == 1 {
                state.bound_addr[0] = 1;
                state.bound_addr[1] = 0;
            } else {
                let port = allocate_ephemeral_port(50000);
                state.bound_addr[0] = 2;
                state.bound_addr[2] = (port >> 8) as u8;
                state.bound_addr[3] = (port & 0xff) as u8;
            }
            state.bound = true;
        }
        state.backend.loopback_mut().peer_addr = Some(dest);
        state.bound_addr
    });
    let listener_pid = listener.with_state(|state| state.owner_pid);
    let (_, listener_uid, listener_gid) = lookup_pid_cred(listener_pid);
    socket.with_state_mut(|state| {
        state.peer_pid = Some(listener_pid);
        state.peer_uid = listener_uid;
        state.peer_gid = listener_gid;
    });
    if sock_type == 2 {
        let listener_weak = Arc::downgrade(&listener);
        socket.with_state_mut(|state| {
            let lb = state.backend.loopback_mut();
            lb.peer_id = Some(listener.socket_id());
            lb.peer_cache = Some(listener_weak);
            lb.peer_addr = Some(dest);
        });
        return Ok(());
    }
    // Establish the connection now, the way Linux completes the handshake at
    // connect() time independent of the server's accept(): create the
    // server-side endpoint, cross-link both directions, and queue the endpoint
    // (not the client) for accept(). This lets the client send immediately —
    // data buffers in the endpoint's recv queue — instead of failing EPIPE
    // because the routed send falls through to smoltcp before accept() linked
    // the peer.
    let (l_domain, l_sock_type, l_proto, listener_addr) =
        listener.with_state(|state| (state.domain, state.sock_type, state.proto, state.bound_addr));
    let server = create_socket_file(l_domain, l_sock_type, l_proto, false);
    crate::dprintln!("[lo] tcp_connect: client_sid={} listener_sid={} server_sid={} port={}", socket.socket_id(), listener.socket_id(), server.socket_id(), port_of(&dest));
    let client_id = socket.socket_id();
    let client_pid = socket.with_state(|state| state.owner_pid);
    let (_, client_uid, client_gid) = lookup_pid_cred(client_pid);
    server.with_state_mut(|state| {
        state.bound_addr = listener_addr;
        state.bound = true;
        let lb = state.backend.loopback_mut();
        lb.peer_id = Some(client_id);
        lb.peer_cache = Some(Arc::downgrade(&socket));
        lb.peer_addr = Some(client_addr);
        state.peer_pid = Some(client_pid);
        state.peer_uid = client_uid;
        state.peer_gid = client_gid;
    });
    let server_id = server.socket_id();
    let server_weak = Arc::downgrade(&server);
    socket.with_state_mut(|state| {
        let lb = state.backend.loopback_mut();
        lb.peer_id = Some(server_id);
        lb.peer_cache = Some(server_weak);
    });
    listener.with_state_mut(|state| {
        state.backend.loopback_mut().accept_queue.push_back(server_id);
        if state.backend.loopback().peer_addr.is_none() {
            state.backend.loopback_mut().peer_addr = Some(client_addr);
        }
    });
    if crate::net::socket::net_trace_enabled() { crate::println!("[TRACE] tcp_connect client_sid={} listener_sid={} server_sid={} port={}", socket.socket_id(), listener.socket_id(), server_id, port_of(&dest)); } // TRACE
    listener.wake_readers();
    Ok(())
}

pub fn sendto(socket: Arc<SocketFile>, data: &[u8], dest: Option<[u8; 16]>) -> Result<usize, SocketError> {
    let t0 = crate::arch::asm_helpers::rdtime(); // [DIAG]
    let owned = data.to_vec();
    let t1 = crate::arch::asm_helpers::rdtime(); // [DIAG]
    let r = sendto_owned(socket, owned, dest).map_err(|(e, _)| e);
    let t2 = crate::arch::asm_helpers::rdtime(); // [DIAG]
    {
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let alloc_us = t1.saturating_sub(t0) * 1_000_000 / freq;
        let send_us = t2.saturating_sub(t1) * 1_000_000 / freq;
        if alloc_us > 300 || send_us > 300 {
            crate::rtdiag_println!("[rtdiag] sendto alloc_us={} send_us={} len={}", alloc_us, send_us, data.len());
        }
    }
    r
}

/// Ownership-taking send: the payload `Vec` is moved straight into the
/// receiver's queue (no per-packet copy). On error the buffer is handed back
/// so a blocking caller can retry after the queue drains without re-copying
/// from userspace.
pub fn sendto_owned(
    socket: Arc<SocketFile>,
    data: Vec<u8>,
    dest: Option<[u8; 16]>,
) -> Result<usize, (SocketError, Vec<u8>)> {
    let (src_addr, peer_id, peer_cache, peer_addr, closed, sock_type) = socket.with_state(|state| {
        let lb = state.backend.loopback();
        (state.bound_addr, lb.peer_id, lb.peer_cache.clone(), lb.peer_addr, state.closed || state.snd_shutdown, state.sock_type)
    });
    if closed {
        return Err((SocketError::BrokenPipe, data));
    }
    // SOCK_STREAM(1) and SOCK_SEQPACKET(5) are connection-oriented:
    // send to the connected peer only (dest is ignored per Linux).
    if sock_type == 1 || sock_type == 5 {
        if let Some(peer_id) = peer_id {
            if let Some(peer) = resolve_peer(&socket, peer_id, peer_cache) {
                // Inline the flow-control check against the peer we already
                // fetched (write_ready() would re-resolve peer_id through the
                // registry and re-lock both sockets on every send).
                // A fully-closed or read-shutdown peer can never drain its
                // queue: fail EPIPE (Linux RSTs / raises EPIPE+SIGPIPE) instead
                // of blocking the sender forever on flow control.
                let (peer_gone, queue_full) = peer.with_state(|state| {
                    let lb = state.backend.loopback();
                    let limit = (state.rcvbuf as usize).max(8 * 1024 * 1024);
                    (state.closed || state.rcv_shutdown, recv_queue_bytes(lb) >= limit)
                });
                if peer_gone {
                    return Err((SocketError::BrokenPipe, data));
                }
                if queue_full {
                    if crate::net::socket::net_trace_enabled() && data.len() <= 64 { crate::println!("[TRACE] sendto WOULDBLOCK sid={} peer={} len={}", socket.socket_id(), peer_id, data.len()); } // TRACE
                    return Err((SocketError::WouldBlock, data));
                }
                if crate::net::socket::net_trace_enabled() && data.len() <= 64 { crate::println!("[TRACE] sendto sid={} -> peer={} len={}", socket.socket_id(), peer_id, data.len()); } // TRACE
                let len = data.len();
                peer.push_recv(data, src_addr);
                return Ok(len);
            }
            crate::println!("[lo] sendto FAIL: sid={} peer_id={} but peer gone!", socket.socket_id(), peer_id);
        }
        if dest.is_some() && peer_addr.is_some() {
            return Ok(data.len());
        }
        crate::println!("[lo] sendto EPIPE: sid={} peer_id={:?}", socket.socket_id(), peer_id);
        return Err((SocketError::BrokenPipe, data));
    }
    if let Some(dest) = dest {
        let target = if socket.with_state(|state| state.domain) == 1 {
            find_bound_unix(&dest, 16, socket.socket_id())
        } else {
            find_bound_receiver(port_of(&dest), socket.socket_id())
        };
        let len = data.len();
        if let Some(target) = target {
            target.push_recv(data, src_addr);
        }
        return Ok(len);
    }
    let len = data.len();
    if let Some(peer_id) = peer_id {
        if let Some(peer) = resolve_peer(&socket, peer_id, peer_cache) {
            peer.push_recv(data, src_addr);
        }
    }
    Ok(len)
}

/// Send data with optional SCM_RIGHTS ancillary file descriptions to peer.
/// `file_descs` contains Arc<FileDescriptor> objects cloned from the sender's fd
/// table — per Linux semantics, SCM_RIGHTS transmits the underlying struct file *,
/// not just fd numbers. An empty vec means no SCM_RIGHTS ancillary data.
pub fn sendmsg_with_fds(socket: Arc<SocketFile>, data: &[u8], file_descs: Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>) -> Result<usize, SocketError> {
    let (src_addr, peer_id, closed, sock_type) = socket.with_state(|state| {
        (state.bound_addr, state.backend.loopback().peer_id, state.closed || state.snd_shutdown, state.sock_type)
    });
    if closed {
        return Err(SocketError::BrokenPipe);
    }
    // SCM_RIGHTS only works on connected stream sockets (SOCK_STREAM/SOCK_SEQPACKET)
    if sock_type != 1 && sock_type != 5 {
        return Err(SocketError::OpNotSupported);
    }
    let peer_id = peer_id.ok_or(SocketError::BrokenPipe)?;
    let peer = peer_socket(&socket).filter(|p| p.socket_id() == peer_id)
        .ok_or(SocketError::BrokenPipe)?;
    // Capture sender credentials for SCM_CREDENTIALS
    let sender_cred = current_ucred();
    // Push data to peer's recv buffer
    peer.push_recv(data.to_vec(), src_addr);
    // Push file descriptions and creds to peer's queues (keep in sync with recv_buf)
    peer.with_state_mut(|state| {
        let lb = state.backend.loopback_mut();
        lb.pending_fds.push_back(file_descs);
        lb.pending_creds.push_back(sender_cred);
    });
    Ok(data.len())
}

/// Get current process's ucred (pid/uid/gid) for SCM_CREDENTIALS.
fn current_ucred() -> Ucred {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    match table.as_ref().and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| (pid, p.uid, p.egid)))) {
        Some((pid, uid, gid)) => Ucred { pid: pid as i32, uid, gid },
        None => Ucred::default(),
    }
}

/// Look up a process's (pid, uid, gid) for SO_PEERCRED / SCM_CREDENTIALS.
fn lookup_pid_cred(pid: usize) -> (usize, u32, u32) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    match table.as_ref().and_then(|t| t.get(pid).map(|p| (p.uid, p.egid))) {
        Some((uid, gid)) => (pid, uid, gid),
        None => (pid, 0, 0),
    }
}

/// Pop received data with associated SCM_RIGHTS file descriptions.
pub fn recvmsg_with_fds(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16], Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>), SocketError> {
    let data_result = socket.pop_loopback_recv();
    match data_result {
        Some((data, addr)) => {
            let file_descs = socket.with_state_mut(|state| {
                state.backend.loopback_mut().pending_fds.pop_front().unwrap_or_default()
            });
            Ok((data, addr, file_descs))
        }
        None if socket.with_state(|state| state.backend.loopback().peer_closed) => {
            Ok((Vec::new(), [0u8; 16], Vec::new()))
        }
        None => Err(SocketError::WouldBlock),
    }
}

/// Pop received data with file descriptions + credentials for SO_PASSCRED sockets.
pub fn recvmsg_with_fds_creds(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16], Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>, Option<Ucred>), SocketError> {
    let data_result = socket.pop_loopback_recv();
    match data_result {
        Some((data, addr)) => {
            let (file_descs, cred) = socket.with_state_mut(|state| {
                let lb = state.backend.loopback_mut();
                let file_descs = lb.pending_fds.pop_front().unwrap_or_default();
                let cred = lb.pending_creds.pop_front();
                (file_descs, cred)
            });
            Ok((data, addr, file_descs, cred))
        }
        None if socket.with_state(|state| state.backend.loopback().peer_closed) => {
            Ok((Vec::new(), [0u8; 16], Vec::new(), None))
        }
        None => Err(SocketError::WouldBlock),
    }
}

pub fn recvfrom(socket: Arc<SocketFile>) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    if let Some(data) = socket.pop_loopback_recv() {
        let peer = peer_socket(&socket);
        if let Some(peer) = peer {
            peer.wake_writers();
        }
        return Ok(data);
    }
    // Buffer drained: if the peer has closed/shut down its write side, the read
    // half is at EOF — return a zero-length datagram so recv()/read() yields 0
    // (Linux EOF) instead of blocking forever.
    if socket.with_state(|state| state.backend.loopback().peer_closed) {
        return Ok((Vec::new(), [0u8; 16]));
    }
    Err(SocketError::WouldBlock)
}

/// Byte-stream variant of [`recvfrom`] for loopback TCP: coalesces queued
/// segments up to `max_len` (see `SocketFile::drain_loopback_stream`) instead of
/// returning exactly one sender write. EOF / WouldBlock handling mirrors
/// `recvfrom`.
pub fn recvfrom_stream(
    socket: Arc<SocketFile>,
    max_len: usize,
) -> Result<(Vec<u8>, [u8; 16]), SocketError> {
    if let Some(data) = socket.drain_loopback_stream(max_len) {
        let peer = peer_socket(&socket);
        if let Some(peer) = peer {
            peer.wake_writers();
        }
        return Ok(data);
    }
    if socket.with_state(|state| state.backend.loopback().peer_closed) {
        return Ok((Vec::new(), [0u8; 16]));
    }
    Err(SocketError::WouldBlock)
}

/// Propagate this socket's close/shutdown-WR to its connected loopback peer:
/// mark the peer's read half at EOF and wake any blocked reader/poller. Mirrors
/// Linux delivering FIN/EOF to the peer when a stream endpoint shuts its write
/// side. No-op for unconnected sockets (no peer_id).
pub(in crate::net) fn notify_peer_closed(socket: &SocketFile) {
    let Some(peer) = peer_socket(socket) else { return };
    peer.with_state_mut(|state| {
        state.backend.loopback_mut().peer_closed = true;
    });
    peer.wake_readers();
}

pub fn peer_addr(socket: &SocketFile) -> Result<[u8; 16], SocketError> {
    socket.with_state(|state| {
        state.backend.loopback().peer_addr.ok_or(SocketError::NotConnected)
    })
}

pub fn shutdown(socket: Arc<SocketFile>, how: i32) -> Result<(), SocketError> {
    // SHUT_WR(1)/SHUT_RDWR(2): our write half is gone — deliver EOF to the peer
    // and fail our own future sends with EPIPE. The read half stays fully
    // usable (Linux only sends a FIN; buffered and in-flight peer data is
    // still receivable), so the socket must NOT be marked closed here.
    // SHUT_RD(0) only affects our own read half, so the peer is unaffected.
    if how != 0 {
        notify_peer_closed(&socket);
        socket.with_state_mut(|state| state.snd_shutdown = true);
        socket.wake_writers();
    }
    if how == 0 || how == 2 {
        socket.with_state_mut(|state| state.rcv_shutdown = true);
        socket.wake_readers();
    }
    if how == 2 {
        socket.mark_closed();
    }
    Ok(())
}

pub fn read_ready(socket: &SocketFile) -> bool {
    socket.with_state(|state| {
        let lb = state.backend.loopback();
        !lb.recv_buf.is_empty()
            || !lb.accept_queue.is_empty()
            // Peer closed with the buffer drained → EOF is readable (recv→0).
            || lb.peer_closed
    })
}

pub fn write_ready(socket: &SocketFile) -> bool {
    let (peer_id, peer_cache, closed, sock_type) = socket.with_state(|state| {
        let lb = state.backend.loopback();
        (lb.peer_id, lb.peer_cache.clone(), state.closed, state.sock_type)
    });
    if closed {
        return false;
    }
    if sock_type != 1 && sock_type != 5 {
        return true;
    }
    let Some(peer_id) = peer_id else {
        return true;
    };
    let Some(peer) = resolve_peer(socket, peer_id, peer_cache) else {
        return false;
    };
    peer.with_state(|state| {
        let lb = state.backend.loopback();
        let limit = (state.rcvbuf as usize).max(8 * 1024 * 1024);
        !state.closed && recv_queue_bytes(lb) < limit
    })
}

fn find_listener(dest_port: u16, exclude_id: usize) -> Option<Arc<SocketFile>> {
    if crate::net::socket::net_trace_enabled() {
        // TRACE: dump all candidates bound to this port
        for s in all_socket_files() {
            let (bound, port, listening, closed, owner) = s.with_state(|st| (st.bound, port_of(&st.bound_addr), st.listening, st.closed, st.owner_pid));
            if port == dest_port {
                crate::println!("[TRACE] find_listener port={} cand sid={} bound={} listening={} closed={} owner={}", dest_port, s.socket_id(), bound, listening, closed, owner);
            }
        }
    }
    let chosen = all_socket_files().into_iter().find(|socket| {
        if socket.socket_id() == exclude_id {
            return false;
        }
        socket.with_state(|state| state.listening && state.bound && port_of(&state.bound_addr) == dest_port && !state.closed)
    });
    if crate::net::socket::net_trace_enabled() { crate::println!("[TRACE] find_listener port={} chosen={:?}", dest_port, chosen.as_ref().map(|s| s.socket_id())); } // TRACE
    chosen
}

fn find_bound_receiver(dest_port: u16, exclude_id: usize) -> Option<Arc<SocketFile>> {
    all_socket_files().into_iter().find(|socket| {
        if socket.socket_id() == exclude_id {
            return false;
        }
        socket.with_state(|state| state.bound && port_of(&state.bound_addr) == dest_port && !state.closed)
    })
}

fn is_local_inet_addr(addr: &[u8; 16]) -> bool {
    addr[4..8] == [0, 0, 0, 0] || addr[4..8] == [127, 0, 0, 1]
}

fn is_unix_path_current_dir(addr: &[u8; 16], addrlen: usize) -> bool {
    addrlen >= 3 && addr[2] == b'.' && (addrlen == 3 || addr[3] == 0)
}

fn unix_path_has_file_prefix(addr: &[u8; 16], addrlen: usize) -> bool {
    if addrlen < 15 {
        return false;
    }
    let prefix = b"dir_enotdir/";
    addr[2..].starts_with(prefix)
}

fn same_unix_addr(a: &[u8; 16], b: &[u8; 16], addrlen: usize) -> bool {
    let len = addrlen.min(16);
    len >= 2 && a[..len] == b[..len]
}

fn find_bound_unix(addr: &[u8; 16], addrlen: usize, exclude_id: usize) -> Option<Arc<SocketFile>> {
    all_socket_files().into_iter().find(|socket| {
        if socket.socket_id() == exclude_id {
            return false;
        }
        socket.with_state(|state| {
            state.domain == 1 && state.bound && !state.closed && same_unix_addr(&state.bound_addr, addr, addrlen)
        })
    })
}

fn find_listener_unix(addr: &[u8; 16], addrlen: usize, exclude_id: usize) -> Option<Arc<SocketFile>> {
    all_socket_files().into_iter().find(|socket| {
        if socket.socket_id() == exclude_id {
            return false;
        }
        socket.with_state(|state| {
            state.domain == 1 && state.listening && state.bound && same_unix_addr(&state.bound_addr, addr, addrlen) && !state.closed
        })
    })
}
