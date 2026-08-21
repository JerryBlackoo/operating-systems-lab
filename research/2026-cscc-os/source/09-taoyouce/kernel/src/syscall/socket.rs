use alloc::sync::Arc;

use super::user::{read_user_buffer, write_user_buffer};
use crate::fs::{fd as fd_table, FileDescriptor, Inode};
use crate::net::socket::{self, SocketError, SocketFile};

/// fd 指向 Socket 类型文件但无 SocketFile 后端（socketpair 的 pipe 端）。
fn is_socketpair_end_fd(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    match crate::syscall::fd::get_fd_obj(fd as usize) {
        Some(f) => {
            f.file.file_type() == crate::fs::FileType::Socket && f.file.as_socket().is_none()
        }
        None => false,
    }
}

pub(crate) fn socket_errno(err: SocketError) -> isize {
    match err {
        SocketError::InvalidArg => -22,
        SocketError::AddrInUse => -98,
        SocketError::ConnectionRefused => -111,
        SocketError::ConnectionReset => -104,
        SocketError::NotConnected => -107,
        SocketError::AlreadyConnected => -106,
        SocketError::WouldBlock => -11,
        SocketError::NoResources => -12,
        SocketError::BadFd => -9,
        SocketError::NotSocket => -88,
        SocketError::BrokenPipe => -32,
        SocketError::OpNotSupported => -95,
        SocketError::NoProtocolOption => -92,
        SocketError::NoEntry => -2,
        SocketError::AddrFamilyNotSupported => -97,
        SocketError::AddrNotAvail => -99,
        SocketError::NotDir => -20,
        SocketError::Timeout => -110,
        SocketError::PermissionDenied => -13,
    }
}

pub(in crate::syscall) fn socket_errno_for_compat(err: SocketError) -> isize {
    socket_errno(err)
}

fn current_pid() -> Option<usize> {
    let pid = crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 { None } else { Some(pid) }
}

fn remember_socket_deadline(pid: usize, deadline: u64) {
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = guard.as_mut() {
        if let Some(process) = table.get_mut(pid) {
            process.waiting_for = Some(crate::sched::process::SOCKET_WAIT_SENTINEL);
            process.wakeup_channel = deadline as usize;
        }
    }
}

fn take_socket_timeout_reentry() -> bool {
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = guard.as_mut() {
        if let Some(pid) = table.current {
            if let Some(process) = table.get_mut(pid) {
                if process.waiting_for == Some(crate::sched::process::SOCKET_WAIT_SENTINEL)
                    && process.wakeup_channel > 1
                    && crate::arch::asm_helpers::rdtime() >= process.wakeup_channel as u64
                {
                    process.clear_wait_state();
                    return true;
                }
            }
        }
    }
    false
}

fn read_socklen(addrlen: *mut u32) -> Result<usize, isize> {
    if addrlen.is_null() {
        return Err(-14isize);
    }
    let bytes = read_user_buffer(addrlen as *const u8, 4).map_err(|_| -14isize)?;
    let raw = u32::from_ne_bytes(bytes[..4].try_into().unwrap());
    // Linux move_addr_to_user(): `get_user(len, ulen); if (len < 0) return -EINVAL`.
    // socklen_t is signed at the syscall boundary, so a negative *addrlen
    // (e.g. fromlen = -1 in recvfrom01/recvmsg01 "invalid socket length")
    // must be rejected with EINVAL rather than silently clamped.
    if (raw as i32) < 0 {
        return Err(-22isize);
    }
    // Linux accepts any non-negative socklen_t; we cap internally at 128 for copy
    Ok((raw as usize).min(128))
}

fn copy_socket_addr(addr: *mut u8, addrlen: *mut u32, socket_addr: &[u8; 16]) -> isize {
    let len = match read_socklen(addrlen) {
        Ok(len) => len,
        Err(errno) => return errno,
    };
    if addr.is_null() {
        return -14;
    }
    let copy_len = len.min(16);
    if copy_len > 0 && write_user_buffer(addr, &socket_addr[..copy_len]).is_err() {
        return -14;
    }
    if write_user_buffer(addrlen as *mut u8, &16u32.to_ne_bytes()).is_err() {
        return -14;
    }
    0
}

fn read_optlen(optlen: *mut u32) -> Result<usize, isize> {
    if optlen.is_null() {
        return Err(-14);
    }
    let bytes = read_user_buffer(optlen as *const u8, 4).map_err(|_| -14isize)?;
    let len = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
    if len > 4096 {
        return Err(-22);
    }
    Ok(len)
}

fn install_socket_fd(socket_file: Arc<SocketFile>, fd_flags: u32, cloexec: bool) -> isize {
    let socket_id = socket_file.socket_id();
    let inode: Arc<dyn Inode> = socket_file;
    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut table) = *table_guard {
        if let Some(pid) = table.current {
            if let Some(process) = table.get_fd_process_mut(pid) {
                let fd = match fd_table::alloc(
                    process,
                    Arc::new(FileDescriptor::new(inode, (fd_flags & !0o3) | 0o2)),
                    0,
                    false,
                ) {
                    Ok(fd) => fd,
                    Err(errno) => {
                        socket::unregister_socket_file(socket_id);
                        return errno;
                    }
                };
                if cloexec {
                    if let Err(errno) = fd_table::set_cloexec(process, fd, true) {
                        return errno;
                    }
                }
                return fd as isize;
            }
        }
    }
    socket::unregister_socket_file(socket_id);
    -9
}

/// socket(198) → 返回 fd
pub(in crate::syscall) fn sys_socket(domain: i32, sock_type: i32, proto: i32) -> isize {
    const SOCK_CLOEXEC:  i32 = 0o2000000;
    const SOCK_NONBLOCK: i32 = 0o4000;
    const AF_UNIX:    i32 = 1;
    const AF_INET:    i32 = 2;
    const AF_INET6:   i32 = 10;
    const AF_PACKET:  i32 = 17;
    const AF_NETLINK: i32 = 16;
    const AF_ALG:     i32 = 38;
    let cloexec = sock_type & SOCK_CLOEXEC != 0;
    let nonblock = sock_type & SOCK_NONBLOCK != 0;
    let real_type = sock_type & 0xf;

    // 真化〔去 AF_PACKET/AF_NETLINK null-fd fake 占位〕：所有受支持的 domain
    // 一律返回真 `SocketFile`（不是 /dev/null fake fd），让 close/dup/
    // getsockopt(SO_DOMAIN/SO_TYPE) 这些走 socket file path 的 round-trip
    // 全部按真 socket 行为响应。后续 send/recv 因为没有真协议栈而走
    // loopback backend，自然报 `-ENOTCONN/-EOPNOTSUPP`——这比 /dev/null
    // 假装 -ENOTSOCK 更接近 Linux 行为。
    //
    // AF_INET6 仍内部降级到 AF_INET（我们没有 IPv6 协议栈，但 socket file
    // 对象本身记录原始 domain，SO_DOMAIN 才能返回原值——见下方 effective_domain
    // 与 record_domain 的分离）。
    let (effective_domain, record_domain) = match domain {
        AF_UNIX | AF_INET => (domain, domain),
        AF_INET6 => (AF_INET, AF_INET6), // 协议栈走 IPv4，但 SO_DOMAIN 报 AF_INET6
        AF_PACKET | AF_NETLINK => (AF_INET, domain), // 走 loopback backend
        AF_ALG => (AF_ALG, AF_ALG),
        _ => return -97, // -EAFNOSUPPORT
    };
    // sock_type 校验：只接受 SOCK_STREAM(1)/SOCK_DGRAM(2)，AF_UNIX 额外
    // 接受 SOCK_SEQPACKET(5)。其它（如 SOCK_RAW=3）按 Linux 行为：
    // root 走真 RAW（我们没真栈所以仍 -EPROTONOSUPPORT），非 root -EPERM。
    // 这里统一返 -EPROTONOSUPPORT(-93) 避免 silent 0。
    // SOCK_RAW (3): allowed for root (euid==0) or CAP_NET_RAW. LTP ICMP/ping tests need this.
    // SOCK_SEQPACKET (5): allowed for AF_UNIX and AF_ALG.
    if real_type != 1 && real_type != 2 && real_type != 3
        && !(effective_domain == AF_UNIX && real_type == 5)
        && !(effective_domain == AF_ALG && real_type == 5)
    {
        return -22; // EINVAL for completely unknown types
    }
    if real_type == 3 {
        // SOCK_RAW: check root/CAP_NET_RAW (like Linux net/ipv4/af_inet.c inet_create)
        let has_cap = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref()
                .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                .map(|p| p.euid == 0 || (p.cap_effective & (1u64 << 13)) != 0) // CAP_NET_RAW=13
                .unwrap_or(false)
        };
        if !has_cap {
            return -1; // EPERM
        }
    }
    if effective_domain == AF_ALG {
        if proto != 0 {
            return -93;
        }
    } else if effective_domain == AF_INET {
        // SOCK_RAW: accept any protocol (IPPROTO_ICMP=1, IPPROTO_RAW=255, etc.)
        if real_type == 3 {
            // Linux inet_create(): a raw socket with protocol 0 (IPPROTO_IP)
            // has no matching raw handler, so socket() fails with
            // EPROTONOSUPPORT even for root. socket01 "raw open as non-root"
            // (PF_INET, SOCK_RAW, 0) expects exactly this. Real raw protocols
            // (ICMP=1, RAW=255, ...) remain accepted.
            if proto == 0 {
                return -93;
            }
        } else if (real_type == 1 && proto != 0 && proto != 6) || (real_type == 2 && proto != 0 && proto != 17) {
            return -93;
        }
    } else if proto != 0 && real_type != 3 {
        return -93;
    }
    let mut fd_flags: u32 = 0;
    if cloexec { fd_flags |= 0o2000000; }
    if nonblock { fd_flags |= 0o4000; }

    let socket_file = socket::create_socket_file(effective_domain, real_type, proto, nonblock);
    if effective_domain == AF_ALG {
        crate::dprintln!("[af_alg] sys_socket domain={} type={} proto={} nonblock={}", domain, real_type, proto, nonblock);
    }
    // 真化〔SO_DOMAIN round-trip〕：对外暴露的 domain 是用户原始请求值
    // （AF_INET6/AF_PACKET/AF_NETLINK），而 effective_domain 只用于
    // backend 路由。这样 getsockopt(SO_DOMAIN) 不会撒谎。
    if record_domain != effective_domain {
        socket::override_record_domain(&socket_file, record_domain);
    }
    install_socket_fd(socket_file, fd_flags, cloexec)
}

/// bind(200)
/// Whether the current process may bind an IPv4/IPv6 privileged port.
///
/// Linux requires `CAP_NET_BIND_SERVICE` to bind a port below
/// `ip_unprivileged_port_start` (default 1024). euid 0 carries the capability
/// implicitly; after a `seteuid` to a non-root user the effective capability set
/// is cleared by the setuid fixup, so the check then falls through to EACCES.
fn current_has_net_bind_service() -> bool {
    const CAP_NET_BIND_SERVICE: u64 = 1u64 << 10;
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| p.euid == 0 || (p.cap_effective & CAP_NET_BIND_SERVICE) != 0)
        .unwrap_or(true)
}

/// Enforce privileged-port permission for AF_INET/AF_INET6 binds. Returns the
/// requested port's privilege violation as `Some(-EACCES)`, or `None` to allow.
fn bind_privileged_port_check(buf: &[u8]) -> Option<isize> {
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 10;
    const IPPORT_RESERVED: u16 = 1024;
    if buf.len() < 4 {
        return None;
    }
    let family = u16::from_ne_bytes([buf[0], buf[1]]);
    if family != AF_INET && family != AF_INET6 {
        return None;
    }
    // sockaddr_in / sockaddr_in6 both carry the port in network byte order at
    // offset 2..4.
    let port = u16::from_be_bytes([buf[2], buf[3]]);
    if port != 0 && port < IPPORT_RESERVED && !current_has_net_bind_service() {
        return Some(-13);
    }
    None
}

pub(in crate::syscall) fn sys_bind(fd: i32, addr: *const u8, addrlen: u32) -> isize {
    if crate::net::socket::is_af_alg_fd(fd) {
        crate::dprintln!("[af_alg] sys_bind fd={} addrlen={}", fd, addrlen);
    }
    if addr.is_null() {
        return -14;
    }
    if addrlen as usize > 128 {
        return -22;
    }
    let len = addrlen as usize;
    let buf = match read_user_buffer(addr, len) {
        Ok(v) => v,
        Err(_) => return -14,
    };
    if let Some(errno) = bind_privileged_port_check(&buf) {
        return errno;
    }
    socket::bind_fd_raw(fd, &buf, len).map_or_else(socket_errno, |_| 0)
}

/// listen(201)
pub(in crate::syscall) fn sys_listen(fd: i32, backlog: i32) -> isize {
    socket::listen_fd(fd, backlog).map_or_else(socket_errno, |_| 0)
}

/// accept(202) — TCP loopback: 从 accept_queue 弹出已连接的 client fd，创建新 socket fd
pub(in crate::syscall) fn sys_accept(fd: i32, addr: *mut u8, addrlen: *mut u32) -> isize {
    let trace_af_alg = crate::net::socket::is_af_alg_fd(fd);
    if trace_af_alg {
        crate::dprintln!("[af_alg] sys_accept fd={}", fd);
    }
    let (accepted, peer_addr) = loop {
        match socket::accept_fd(fd) {
            Ok(result) => break result,
            Err(SocketError::WouldBlock) => {
                if trace_af_alg {
                    crate::dprintln!("[af_alg] sys_accept wouldblock fd={}", fd);
                }
                if let Some(pid) = current_pid() {
                    let _ = socket::register_read_waiter_fd_after_wouldblock(fd, pid, None);
                }
                match socket::accept_fd(fd) {
                    Ok(result) => break result,
                    Err(SocketError::WouldBlock) => {}
                    Err(err) => return socket_errno(err),
                }
                // Slice 2.5 wchan：accept 阻塞 → Linux `inet_csk_accept`
                crate::sched::process::enter_wait_channel("inet_csk_accept");
                crate::sched::process::suspend_current_and_run_next();
            }
            Err(err) => return socket_errno(err),
        }
    };
    let new_fd = install_socket_fd(accepted, 0, false);
    if new_fd < 0 {
        return new_fd;
    }
    if !addr.is_null() {
        let _ = write_user_buffer(addr, &peer_addr);
    }
    if !addrlen.is_null() {
        let _ = write_user_buffer(addrlen as *mut u8, &16u32.to_ne_bytes());
    }
    new_fd
}

pub(crate) fn sys_accept4(fd: i32, addr: *mut u8, addrlen: *mut u32, flags: i32) -> isize {
    const SOCK_CLOEXEC: i32 = 0o2000000;
    const SOCK_NONBLOCK: i32 = 0o4000;
    if flags & !(SOCK_CLOEXEC | SOCK_NONBLOCK) != 0 {
        return -22;
    }
    if flags == 0 {
        return sys_accept(fd, addr, addrlen);
    }
    do_accept(fd, addr, addrlen, flags)
}

fn do_accept(fd: i32, addr: *mut u8, addrlen: *mut u32, flags: i32) -> isize {
    const SOCK_CLOEXEC: i32 = 0o2000000;
    const SOCK_NONBLOCK: i32 = 0o4000;
    let (accepted, peer_addr) = loop {
        match socket::accept_fd(fd) {
            Ok(result) => break result,
            Err(SocketError::WouldBlock) => {
                if let Some(pid) = current_pid() {
                    let _ = socket::register_read_waiter_fd_after_wouldblock(fd, pid, None);
                }
                match socket::accept_fd(fd) {
                    Ok(result) => break result,
                    Err(SocketError::WouldBlock) => {}
                    Err(err) => return socket_errno(err),
                }
                // Slice 2.5 wchan：accept4 阻塞 → 同 sys_accept
                crate::sched::process::enter_wait_channel("inet_csk_accept");
                crate::sched::process::suspend_current_and_run_next();
            }
            Err(err) => return socket_errno(err),
        }
    };
    let nonblock = flags & SOCK_NONBLOCK != 0;
    let cloexec = flags & SOCK_CLOEXEC != 0;
    if nonblock {
        socket::set_socket_nonblocking(&accepted, true);
    }
    let mut fd_flags = 0u32;
    if nonblock {
        fd_flags |= SOCK_NONBLOCK as u32;
    }
    if cloexec {
        fd_flags |= SOCK_CLOEXEC as u32;
    }
    let new_fd = install_socket_fd(accepted, fd_flags, cloexec);
    if new_fd < 0 {
        return new_fd;
    }
    if !addr.is_null() {
        let _ = write_user_buffer(addr, &peer_addr);
    }
    if !addrlen.is_null() {
        let _ = write_user_buffer(addrlen as *mut u8, &16u32.to_ne_bytes());
    }
    new_fd
}

/// connect(203) — TCP loopback: 查找 listening socket 配对
pub(crate) fn sys_connect(fd: i32, addr: *const u8, addrlen: u32) -> isize {
    // Per Linux net/socket.c __sys_connect: validate fd is a socket first
    if let Err(err) = socket::validate_socket_fd(fd) {
        return socket_errno(err);
    }
    if addr.is_null() {
        return -14;
    }
    // Linux allows addrlen >= 2 (just need sa_family)
    if addrlen < 2 {
        return -22;
    }
    let len = (addrlen as usize).min(16);
    let mut dest = [0u8; 16];
    match read_user_buffer(addr, len) {
        Ok(v) => dest[..len].copy_from_slice(&v[..len]),
        Err(_) => return -14,
    }
    let family = u16::from_ne_bytes([dest[0], dest[1]]);
    // AF_UNSPEC (0) = disconnect for UDP, AF_UNIX (1), AF_INET (2), AF_INET6 (10),
    // AF_NETLINK (16), AF_PACKET (17) — all acceptable families
    match family {
        // AF_INET: Linux tcp_v4_connect()/udp connect require
        // addr_len >= sizeof(struct sockaddr_in) (16); connect01 "invalid
        // salen" passes salen=3 and expects EINVAL.
        2 => if (addrlen as usize) < 16 { return -22; },
        // AF_INET6: sizeof(struct sockaddr_in6) == 28.
        10 => if (addrlen as usize) < 28 { return -22; },
        0 | 1 | 16 | 17 => {}
        _ => return -97, // EAFNOSUPPORT
    }
    socket::connect_fd(fd, dest).map_or_else(socket_errno, |_| 0)
}

/// getsockname(204)
pub(in crate::syscall) fn sys_getsockname(fd: i32, addr: *mut u8, addrlen: *mut u32) -> isize {
    let local_addr = match socket::getsockname_fd(fd) {
        Ok(addr) => addr,
        Err(SocketError::BadFd) if crate::syscall::ipc::fd_is_unix_socketpair(fd) => {
            unnamed_unix_addr()
        }
        Err(err) => return socket_errno(err),
    };
    copy_socket_addr(addr, addrlen, &local_addr)
}

/// An unnamed AF_UNIX address (`sa_family = AF_UNIX`, no path). Returned by
/// getsockname/getpeername for socketpair endpoints (getpeername01).
fn unnamed_unix_addr() -> [u8; 16] {
    let mut a = [0u8; 16];
    a[0] = 1; // AF_UNIX, little-endian sa_family low byte
    a
}

pub(in crate::syscall) fn sys_getpeername(fd: i32, addr: *mut u8, addrlen: *mut u32) -> isize {
    let peer_addr = match socket::getpeername_fd(fd) {
        Ok(addr) => addr,
        Err(SocketError::BadFd) if crate::syscall::ipc::fd_is_unix_socketpair(fd) => {
            // A socketpair endpoint is always connected to its unnamed peer.
            unnamed_unix_addr()
        }
        Err(err) => return socket_errno(err),
    };
    copy_socket_addr(addr, addrlen, &peer_addr)
}

/// sendto(206) — per Linux net/socket.c __sys_sendto
pub(crate) fn sys_sendto(fd: i32, buf: *const u8, len: usize, flags: i32,
              dest_addr: *const u8, addrlen: u32) -> isize {
    // Resolve fd → socket exactly once (Linux sockfd_lookup_light does the
    // same); every later step reuses the Arc instead of re-walking the
    // process table + fd table + socket registry.
    let sock = match socket::socket_from_fd(fd) {
        Ok(sock) => sock,
        Err(err) => {
            // socketpair(AF_UNIX) fd（pipe 实现的 UnixSocketPairEnd，无
            // SocketFile 后端）：Linux send(2) 对任何已连接 socket 有效，
            // 语义等价 write(2)（send 页原话），转交 write 路径。
            if is_socketpair_end_fd(fd) {
                let _ = (dest_addr, addrlen); // 已连接：Linux 忽略 dest
                return crate::fs::write_io::sys_write(fd as usize, buf, len);
            }
            return socket_errno(err);
        }
    };
    // Accept common flags: MSG_DONTWAIT, MSG_NOSIGNAL, MSG_MORE, MSG_EOR,
    // MSG_CONFIRM, MSG_DONTROUTE, MSG_OOB, MSG_CMSG_CLOEXEC
    // Per Linux net/socket.c, unknown flags are silently accepted on send path.
    if len > 65507 {
        return -90; // EMSGSIZE
    }
    let data = if len == 0 {
        alloc::vec::Vec::new()
    } else if len > 4 * 4096 {
        // >4 页时 read_user_to_slice 会退化为 read_user_buffer 中转 + 二次
        // 全量拷贝；直接 read_user_buffer 拿 Vec 保持单次 user→kernel 拷贝。
        match super::user::read_user_buffer(buf, len) {
            Ok(v) => v,
            Err(_) => return -14,
        }
    } else {
        let mut v = crate::net::bufpool::take(len);
        match super::user::read_user_to_slice(buf, &mut v) {
            Ok(_) => v,
            Err(_) => return -14,
        }
    };

    if addrlen as usize > 128 {
        return -22;
    }
    let alen = (addrlen as usize).min(16);
    let dest = if !dest_addr.is_null() && alen > 0 {
        let mut dest = [0u8; 16];
        match read_user_buffer(dest_addr, alen) {
            Ok(v) => dest[..alen].copy_from_slice(&v[..alen]),
            Err(_) => return -14,
        }
        Some(dest)
    } else {
        None
    };

    // MSG_OOB: TCP out-of-band data — handle before normal send path
    const MSG_OOB: i32 = 0x1;
    if flags & MSG_OOB != 0 {
        // MSG_OOB is only meaningful for connection-oriented (SOCK_STREAM)
        // sockets. On a datagram socket Linux returns EOPNOTSUPP
        // (sendto01 "invalid flags set" on a UDP socket).
        if sock.sock_type() != 1 {
            return -95; // EOPNOTSUPP
        }
        return match crate::net::tcp::tcp_send_oob(&sock, &data) {
            Ok(n) => n as isize,
            Err(err) => socket_errno(err),
        };
    }

    // MSG_MORE: 把本次数据 cork 起来不下发, 待一次不带 MSG_MORE 的 send 整体下发。
    let data_len = data.len();
    let send_buf = match socket::take_cork_or_buffer_owned(&sock, data, flags) {
        Ok(None) => return data_len as isize, // 已 cork, 报告本次长度
        Ok(Some(buf)) => buf,
        Err(err) => return socket_errno(err),
    };
    // cork 被 flush 时(buf 比本次 data 长)按 Linux 语义返回**本次** data 的长度,
    // 而非合并后的总长度; 普通无 cork 路径仍返回后端实际发送字节数。
    let flushed_cork = send_buf.len() != data_len;
    let ret_len = data_len;

    // 阻塞模式：如果 send buffer 满（WouldBlock），循环重试
    // 对标 Linux: blocking socket 的 sendto/write 会阻塞直到有空间。
    // payload 以所有权在重试间来回传递：成功时 move 进接收队列（零拷贝），
    // WouldBlock 时后端原样归还，无需重新从用户态拷贝。
    let mut payload = send_buf;
    loop {
        match socket::sendto_sock_owned(&sock, payload, dest) {
            Ok(n) => return if flushed_cork { ret_len as isize } else { n as isize },
            Err((SocketError::WouldBlock, buf_back)) => {
                payload = buf_back;
                if flags & 0x40 != 0 { // MSG_DONTWAIT
                    return -11; // EAGAIN
                }
                // 注册写等待并挂起
                if let Some(pid) = current_pid() {
                    socket::register_write_waiter_sock_after_wouldblock(&sock, pid, None);
                }
                match socket::sendto_sock_owned(&sock, payload, dest) {
                    Ok(n) => return if flushed_cork { ret_len as isize } else { n as isize },
                    Err((SocketError::WouldBlock, buf_back)) => payload = buf_back,
                    Err((err, _)) => return socket_errno(err),
                }
                crate::sched::process::enter_wait_channel("tcp_sendmsg");
                crate::sched::process::suspend_current_and_run_next();
            }
            Err((err, _)) => return socket_errno(err),
        }
    }
}

/// recvfrom(207) — per Linux net/socket.c __sys_recvfrom
pub(crate) fn sys_recvfrom(fd: i32, buf: *mut u8, len: usize, flags: i32,
                src_addr: *mut u8, addrlen: *mut u32) -> isize {
    // Resolve fd → socket exactly once (Linux sockfd_lookup_light); every
    // later step reuses the Arc instead of re-walking the process table +
    // fd table + socket registry.
    let sock = match socket::socket_from_fd(fd) {
        Ok(sock) => sock,
        Err(err) => {
            // socketpair(AF_UNIX) fd（pipe 实现，无 SocketFile 后端）：
            // Linux recv(2) 对任何已连接 socket 有效，等价 read(2)（recv 页
            // 原话，flags==0 时）。rustc(std::process spawn) 用 socketpair 做
            // CLOEXEC 报错管道并以 recvfrom 读取。
            if is_socketpair_end_fd(fd) {
                if !src_addr.is_null() && !addrlen.is_null() {
                    // 无名对端：addrlen=0（Linux unix_getname 对 unnamed peer）
                    let zero = 0u32;
                    let _ = super::user::write_user_buffer(
                        addrlen as *mut u8, &zero.to_le_bytes());
                }
                return crate::fs::read_io::sys_read(fd as usize, buf, len);
            }
            return socket_errno(err);
        }
    };
    let trace_af_alg = sock.domain() == 38;
    if trace_af_alg {
        crate::dprintln!("[af_alg] sys_recvfrom fd={} len={} flags={:#x}", fd, len, flags);
    }
    const MSG_OOB: i32 = 0x1;
    const MSG_ERRQUEUE: i32 = 0x2000;
    // MSG_ERRQUEUE: return EAGAIN (no error queue in our implementation)
    if flags & MSG_ERRQUEUE != 0 {
        return -11;
    }
    // MSG_OOB: TCP out-of-band data recv — per Linux net/ipv4/tcp.c tcp_recvmsg()
    if flags & MSG_OOB != 0 {
        let mut oob_buf = [0u8; 1];
        return match crate::net::tcp::tcp_recv_oob(&sock, &mut oob_buf) {
            Ok(n) => {
                if n > 0 && !buf.is_null() && len > 0 {
                    if write_user_buffer(buf, &oob_buf[..n]).is_err() {
                        return -14;
                    }
                }
                n as isize
            }
            Err(_) => -22, // EINVAL: no OOB data pending
        };
    }
    if take_socket_timeout_reentry() {
        return -11;
    }
    let recv_deadline = if flags & 0x40 != 0 {
        None
    } else {
        socket::recv_timeout_deadline_sock(&sock)
    };
    let user_addrlen = if !addrlen.is_null() {
        match read_socklen(addrlen) {
            Ok(len) => len as usize,
            Err(errno) => return errno,
        }
    } else {
        0usize
    };
    // SOCK_STREAM (loopback TCP) coalesces queued sender writes up to the user
    // buffer (Linux byte-stream recv); a peeking recv must not consume, so it
    // stays on the single-segment path.
    const MSG_PEEK: i32 = 0x2;
    let recvfrom_for = |sock: &alloc::sync::Arc<crate::net::socket::SocketFile>| {
        if flags & MSG_PEEK != 0 {
            socket::recvfrom_sock(sock)
        } else {
            socket::recvfrom_sock_coalesce(sock, len)
        }
    };
    let (data, from) = loop {
        match recvfrom_for(&sock) {
            Ok(result) => break result,
            Err(SocketError::WouldBlock) => {
                if trace_af_alg {
                    crate::dprintln!("[af_alg] sys_recvfrom wouldblock fd={} len={} flags={:#x}", fd, len, flags);
                }
                if flags & 0x40 != 0 {
                    return -11;
                }
                if recv_deadline.map_or(false, |deadline| crate::arch::asm_helpers::rdtime() >= deadline) {
                    return -11;
                }
                if let Some(pid) = current_pid() {
                    socket::register_read_waiter_sock_after_wouldblock(&sock, pid, recv_deadline);
                    if let Some(deadline) = recv_deadline {
                        remember_socket_deadline(pid, deadline);
                    }
                }
                match recvfrom_for(&sock) {
                    Ok(result) => break result,
                    Err(SocketError::WouldBlock) => {}
                    Err(err) => return socket_errno(err),
                }
                // Slice 2.5 wchan：recvfrom 阻塞 → Linux `sock_recvmsg`
                crate::sched::process::enter_wait_channel("sock_recvmsg");
                if let Some(deadline) = recv_deadline {
                    crate::sched::process::suspend_current_for_io(deadline);
                } else {
                    crate::sched::process::suspend_current_and_run_next();
                }
            }
            Err(err) => return socket_errno(err),
        }
    };
    let original_len = data.len();
    let copy_len = len.min(original_len);
    if write_user_buffer(buf, &data[..copy_len]).is_err() {
        return -14;
    }
    if !src_addr.is_null() && user_addrlen > 0 {
        let write_len = user_addrlen.min(from.len());
        let _ = write_user_buffer(src_addr, &from[..write_len]);
    }
    if !addrlen.is_null() {
        // Per Linux: update addrlen to actual address length (16 for sockaddr_in)
        let actual_len = 16u32.min(user_addrlen as u32);
        let _ = write_user_buffer(addrlen as *mut u8, &actual_len.to_ne_bytes());
    }
    // Per Linux: for SOCK_DGRAM/SOCK_SEQPACKET with MSG_TRUNC, return the
    // original datagram length even if it exceeds the receive buffer size.
    const MSG_TRUNC: i32 = 0x20;
    crate::net::bufpool::put(data);
    if flags & MSG_TRUNC != 0 {
        return original_len as isize;
    }
    copy_len as isize
}

// ---- recvmsg / sendmsg / recvmmsg / sendmmsg ------------------------------
//
// RV64/LA64 Linux layout:
//   struct msghdr {
//     void *msg_name;        (8)
//     socklen_t msg_namelen; (4) + 4 pad
//     struct iovec *msg_iov; (8)
//     size_t msg_iovlen;     (8)
//     void *msg_control;     (8)
//     size_t msg_controllen; (8)
//     int msg_flags;         (4) + 4 pad
//   }
fn parse_msghdr(msg: *const u8) -> Option<(usize, usize, *mut u8, usize)> {
    let bytes = read_user_buffer(msg, 56).ok()?;
    let iov_ptr = usize::from_le_bytes(bytes[16..24].try_into().ok()?);
    let iov_len = usize::from_le_bytes(bytes[24..32].try_into().ok()?);
    let ctl_ptr = usize::from_le_bytes(bytes[32..40].try_into().ok()?);
    let ctl_len = usize::from_le_bytes(bytes[40..48].try_into().ok()?);
    Some((iov_ptr, iov_len, ctl_ptr as *mut u8, ctl_len))
}

pub(crate) fn sys_recvmsg(fd: i32, msg: *mut u8, flags: i32) -> isize {
    let trace_af_alg = crate::net::socket::is_af_alg_fd(fd);
    if trace_af_alg {
        crate::dprintln!("[af_alg] sys_recvmsg fd={} flags={:#x}", fd, flags);
    }
    // Linux __copy_msghdr(): when msg_name != NULL, a negative msg_namelen is
    // rejected with EINVAL (recvmsg01 "invalid socket length" sets
    // msg_namelen = -1 with a non-NULL msg_name). msg_name @ off 0, namelen @ off 8.
    if let Ok(hdr) = read_user_buffer(msg as *const u8, 12) {
        let name_ptr = u64::from_ne_bytes(hdr[0..8].try_into().unwrap());
        let namelen = i32::from_ne_bytes(hdr[8..12].try_into().unwrap());
        if name_ptr != 0 && namelen < 0 {
            return -22;
        }
    }
    let (iov_ptr, iov_cnt, ctl_ptr, ctl_len) = match parse_msghdr(msg as *const u8) {
        Some(parsed) => parsed,
        None => return -14,
    };
    if trace_af_alg {
        crate::dprintln!("[af_alg] sys_recvmsg parsed iov_cnt={} iov_ptr={:#x}", iov_cnt, iov_ptr);
    }
    if iov_cnt == 0 { return 0; }
    if iov_cnt > 1024 { return -90; }

    // Check if this is an AF_UNIX socket that might have SCM_RIGHTS
    let is_unix = crate::net::socket::is_unix_fd(fd);
    if is_unix {
        return sys_recvmsg_unix(fd, msg, flags, iov_ptr, iov_cnt, ctl_ptr, ctl_len);
    }

    let iov_bytes = match read_user_buffer(iov_ptr as *const u8, iov_cnt * 16) {
        Ok(bytes) => bytes,
        Err(_) => return -14,
    };
    let mut total = 0isize;
    for i in 0..iov_cnt {
        let base = usize::from_le_bytes(iov_bytes[i * 16..i * 16 + 8].try_into().unwrap()) as *mut u8;
        let len = usize::from_le_bytes(iov_bytes[i * 16 + 8..i * 16 + 16].try_into().unwrap());
        let n = sys_recvfrom(fd, base, len, flags, core::ptr::null_mut(), core::ptr::null_mut());
        if n < 0 { return if total > 0 { total } else { n }; }
        if n == 0 { break; }
        total += n;
        if (n as usize) < len { break; }
    }
    total
}

/// AF_UNIX recvmsg with SCM_RIGHTS + SCM_CREDENTIALS support.
/// Receives data + ancillary fds + sender ucred, installs received fds into
/// caller's fd table, writes cmsg(s) to msg_control.
fn sys_recvmsg_unix(
    fd: i32,
    msg: *mut u8,
    _flags: i32,
    iov_ptr: usize,
    iov_cnt: usize,
    ctl_ptr: *mut u8,
    ctl_len: usize,
) -> isize {
    use crate::net::socket;
    use crate::net::backend;

    let passcred = socket::is_passcred_fd(fd);

    let sock = match socket::socket_from_fd(fd) {
        Ok(s) => s,
        Err(_) => return -9,
    };

    // Receive with fds + credentials
    let (data, _addr, fds, cred) = match backend::recvmsg_unix_creds(sock) {
        Ok(result) => result,
        Err(crate::net::socket::SocketError::WouldBlock) => return -11,
        Err(_) => return -104,
    };

    // Copy data to iov buffers
    let iov_bytes = match read_user_buffer(iov_ptr as *const u8, iov_cnt * 16) {
        Ok(bytes) => bytes,
        Err(_) => return -14,
    };
    let mut data_offset = 0usize;
    for i in 0..iov_cnt {
        if data_offset >= data.len() { break; }
        let base = usize::from_le_bytes(iov_bytes[i * 16..i * 16 + 8].try_into().unwrap()) as *mut u8;
        let len = usize::from_le_bytes(iov_bytes[i * 16 + 8..i * 16 + 16].try_into().unwrap());
        let copy_len = len.min(data.len() - data_offset);
        if write_user_buffer(base, &data[data_offset..data_offset + copy_len]).is_err() {
            return -14;
        }
        data_offset += copy_len;
    }

    // Build ancillary control messages
    if !ctl_ptr.is_null() && ctl_len > 0 {
        let mut cmsg_buf = alloc::vec::Vec::<u8>::new();

        // SCM_RIGHTS cmsg
        if !fds.is_empty() {
            let new_fds = install_received_fds(&fds);
            if !new_fds.is_empty() {
                let cmsg_data_len = new_fds.len() * 4;
                let cmsg_len = 16 + cmsg_data_len;
                let cmsg_space = (cmsg_len + 7) & !7;
                let base = cmsg_buf.len();
                cmsg_buf.resize(base + cmsg_space, 0);
                cmsg_buf[base..base + 8].copy_from_slice(&(cmsg_len as u64).to_le_bytes());
                cmsg_buf[base + 8..base + 12].copy_from_slice(&1i32.to_le_bytes()); // SOL_SOCKET
                cmsg_buf[base + 12..base + 16].copy_from_slice(&1i32.to_le_bytes()); // SCM_RIGHTS
                for (i, &new_fd) in new_fds.iter().enumerate() {
                    let off = base + 16 + i * 4;
                    cmsg_buf[off..off + 4].copy_from_slice(&new_fd.to_le_bytes());
                }
            }
        }

        // SCM_CREDENTIALS cmsg (struct ucred: pid i32, uid u32, gid u32 = 12 bytes)
        if passcred {
            let uc = cred.unwrap_or_default();
            let cmsg_len: usize = 16 + 12; // cmsghdr(16) + ucred(12)
            let cmsg_space = (cmsg_len + 7) & !7; // 32
            let base = cmsg_buf.len();
            cmsg_buf.resize(base + cmsg_space, 0);
            cmsg_buf[base..base + 8].copy_from_slice(&(cmsg_len as u64).to_le_bytes());
            cmsg_buf[base + 8..base + 12].copy_from_slice(&1i32.to_le_bytes()); // SOL_SOCKET
            cmsg_buf[base + 12..base + 16].copy_from_slice(&2i32.to_le_bytes()); // SCM_CREDENTIALS
            cmsg_buf[base + 16..base + 20].copy_from_slice(&uc.pid.to_le_bytes());
            cmsg_buf[base + 20..base + 24].copy_from_slice(&uc.uid.to_le_bytes());
            cmsg_buf[base + 24..base + 28].copy_from_slice(&uc.gid.to_le_bytes());
        }

        if !cmsg_buf.is_empty() && ctl_len >= cmsg_buf.len() {
            let _ = write_user_buffer(ctl_ptr, &cmsg_buf);
            let controllen_bytes = (cmsg_buf.len() as u64).to_le_bytes();
            let _ = write_user_buffer(unsafe { (msg as *mut u8).add(40) }, &controllen_bytes);
        } else {
            let zero = 0u64.to_le_bytes();
            let _ = write_user_buffer(unsafe { (msg as *mut u8).add(40) }, &zero);
        }
    }

    data_offset as isize
}

/// Resolve sender fd numbers to Arc<FileDescriptor> objects from current process's
/// fd table. Called in the sendmsg path so that SCM_RIGHTS transmits the actual
/// underlying file description (like Linux's struct file *), not just fd indices.
fn resolve_fds_to_file_descs(fd_nums: &[i32]) -> alloc::vec::Vec<alloc::sync::Arc<crate::fs::FileDescriptor>> {
    use alloc::vec::Vec;
    let mut descs = Vec::new();
    if fd_nums.is_empty() { return descs; }
    let guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_ref() { Some(t) => t, None => return descs };
    let cur_pid = match table.current { Some(p) => p, None => return descs };
    let proc = match table.get(cur_pid) { Some(p) => p, None => return descs };

    for &fd in fd_nums {
        let fd_idx = fd as usize;
        if fd_idx < proc.fd_table.len() {
            if let Some(ref file_desc) = proc.fd_table[fd_idx] {
                descs.push(file_desc.clone());
            }
        }
    }
    descs
}

/// Install received Arc<FileDescriptor> objects into current process's fd table.
/// Per Linux semantics, each received file description is installed as a new fd
/// in the receiver's table, returning the newly allocated fd numbers.
fn install_received_fds(file_descs: &[alloc::sync::Arc<crate::fs::FileDescriptor>]) -> alloc::vec::Vec<i32> {
    use alloc::vec::Vec;
    let mut new_fds = Vec::new();
    if file_descs.is_empty() { return new_fds; }
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return new_fds };
    let cur_pid = match table.current { Some(p) => p, None => return new_fds };
    let proc = match table.get_mut(cur_pid) { Some(p) => p, None => return new_fds };

    for file_desc in file_descs {
        // Find lowest available fd slot
        let new_fd_num = proc.fd_table.iter().position(|slot| slot.is_none())
            .unwrap_or_else(|| { proc.fd_table.push(None); proc.fd_table.len() - 1 });
        proc.fd_table[new_fd_num] = Some(file_desc.clone());
        new_fds.push(new_fd_num as i32);
    }
    new_fds
}

pub(crate) fn sys_sendmsg(fd: i32, msg: *const u8, flags: i32) -> isize {
    if fd < 0 { return -9; }
    let trace_af_alg = crate::net::socket::is_af_alg_fd(fd);
    if trace_af_alg {
        crate::dprintln!("[af_alg] sys_sendmsg fd={} flags={:#x}", fd, flags);
    }
    let (iov_ptr, iov_cnt, ctl_ptr, ctl_len) = match parse_msghdr(msg) {
        Some(parsed) => parsed,
        None => return -14,
    };
    if trace_af_alg {
        crate::dprintln!("[af_alg] sys_sendmsg parsed iov_cnt={} ctl_len={} ctl_ptr={:#x}", iov_cnt, ctl_len, ctl_ptr as usize);
    }
    if iov_cnt == 0 { return 0; }
    if iov_cnt > 1024 { return -90; }
    let iov_bytes = match read_user_buffer(iov_ptr as *const u8, iov_cnt * 16) {
        Ok(bytes) => bytes,
        Err(_) => return -14,
    };
    let mut data = alloc::vec::Vec::new();
    for i in 0..iov_cnt {
        let base = usize::from_le_bytes(iov_bytes[i * 16..i * 16 + 8].try_into().unwrap()) as *const u8;
        let len = usize::from_le_bytes(iov_bytes[i * 16 + 8..i * 16 + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let chunk = match read_user_buffer(base, len) {
            Ok(chunk) => chunk,
            Err(_) => return -14,
        };
        data.extend_from_slice(&chunk);
    }
    let control = if ctl_len == 0 {
        None
    } else {
        if ctl_ptr.is_null() {
            return -14;
        }
        match read_user_buffer(ctl_ptr as *const u8, ctl_len) {
            Ok(control) => Some(control),
            Err(_) => return -14,
        }
    };
    // For AF_UNIX with SCM_RIGHTS, resolve sender fd numbers to Arc<FileDescriptor>
    // and pass the actual file descriptions through the socket — per Linux semantics,
    // SCM_RIGHTS transmits the underlying struct file *, not just fd numbers.
    let is_unix = crate::net::socket::is_unix_fd(fd);
    if is_unix {
        use crate::net::backend;
        let fd_nums = backend::parse_scm_rights_pub(control.as_deref());
        let file_descs = resolve_fds_to_file_descs(&fd_nums);
        let sock = match crate::net::socket::socket_from_fd(fd) {
            Ok(s) => s,
            Err(_) => return -9,
        };
        return backend::sendmsg_unix_with_file_descs(sock, &data, file_descs)
            .map_or_else(socket_errno, |n| n as isize);
    }
    // AF_INET datagram/stream（非 AF_ALG）：sendmsg 须按 msg_name 投递（未连接 UDP
    // 经 msg_name 指定目的地，否则 ENOTCONN），并支持 MSG_MORE corking——与
    // sendto 走同一语义（send02 的 UDP sendmsg + MSG_MORE）。AF_ALG 仍走 sendmsg_fd
    // （依赖 flags/control 驱动加解密，不可改道）。
    if !crate::net::socket::is_af_alg_fd(fd) {
        // 解析 msg_name → dest（sockaddr，最多 16 字节）。
        let dest = match read_user_buffer(msg, 12) {
            Ok(hdr) => {
                let name_ptr = u64::from_ne_bytes(hdr[0..8].try_into().unwrap()) as usize;
                let name_len = u32::from_ne_bytes(hdr[8..12].try_into().unwrap()) as usize;
                if name_ptr != 0 && name_len > 0 {
                    let alen = name_len.min(16);
                    match read_user_buffer(name_ptr as *const u8, alen) {
                        Ok(v) => {
                            let mut d = [0u8; 16];
                            d[..alen].copy_from_slice(&v[..alen]);
                            Some(d)
                        }
                        Err(_) => return -14,
                    }
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        let send_buf = match socket::take_cork_or_buffer(fd, &data, flags) {
            Ok(None) => return data.len() as isize,
            Ok(Some(buf)) => buf,
            Err(err) => return socket_errno(err),
        };
        let flushed_cork = send_buf.len() != data.len();
        let ret_len = data.len();
        return match socket::sendto_fd(fd, &send_buf, dest) {
            Ok(n) => if flushed_cork { ret_len as isize } else { n as isize },
            Err(err) => socket_errno(err),
        };
    }
    socket::sendmsg_fd(fd, &data, flags, control.as_deref())
        .map_or_else(socket_errno, |n| n as isize)
}

/// recvmmsg / sendmmsg = struct mmsghdr { struct msghdr msg_hdr; unsigned int msg_len; }
pub(in crate::syscall) fn sys_recvmmsg(
    fd: i32,
    mmsg: *mut u8,
    vlen: u32,
    flags: i32,
    timeout: *const u8,
) -> isize {
    const MSG_DONTWAIT: i32 = 0x40;
    const MSG_WAITFORONE: i32 = 0x10000;
    if vlen == 0 { return 0; }

    let deadline_ticks: Option<u64> = if timeout.is_null() {
        None
    } else {
        match read_user_buffer(timeout, 16) {
            Ok(bytes) => {
                let sec = i64::from_le_bytes(bytes[0..8].try_into().unwrap());
                let nsec = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
                // Linux validates the timeout up-front: a negative seconds or an
                // out-of-range nanoseconds value yields EINVAL (recvmmsg01).
                if sec < 0 || nsec < 0 || nsec >= 1_000_000_000 {
                    return -22;
                }
                let sec = sec as u64;
                let nsec = nsec as u64;
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                let ticks = sec.saturating_mul(freq)
                    .saturating_add((nsec as u128 * freq as u128 / 1_000_000_000u128) as u64);
                Some(crate::arch::asm_helpers::rdtime().saturating_add(ticks))
            }
            Err(_) => return -14,
        }
    };

    let waitforone = (flags & MSG_WAITFORONE) != 0;
    let base_flags = flags & !MSG_WAITFORONE;
    let mut current_flags = base_flags;
    let mut processed = 0i32;
    for i in 0..vlen {
        // Always process the first message before honoring the deadline so that
        // an immediate (zero) timeout still surfaces the underlying recvmsg
        // error (EBADF for a bad fd, EFAULT for a bad vector) — recvmmsg01.
        let entry = unsafe { mmsg.add((i as usize) * 64) };
        let result = sys_recvmsg(fd, entry, current_flags);
        if result < 0 {
            return if processed > 0 { processed as isize } else { result };
        }
        let n_u32 = (result as u32).to_le_bytes();
        let _ = write_user_buffer(unsafe { entry.add(56) }, &n_u32);
        processed += 1;
        if result == 0 { break; }
        if waitforone {
            current_flags |= MSG_DONTWAIT;
        }
        if let Some(deadline) = deadline_ticks {
            if crate::arch::asm_helpers::rdtime() >= deadline {
                break;
            }
        }
    }
    processed as isize
}

pub(in crate::syscall) fn sys_sendmmsg(fd: i32, mmsg: *mut u8, vlen: u32, flags: i32) -> isize {
    if vlen == 0 { return 0; }
    let mut processed = 0i32;
    for i in 0..vlen {
        let entry = unsafe { mmsg.add((i as usize) * 64) };
        let result = sys_sendmsg(fd, entry as *const u8, flags);
        if result < 0 { return if processed > 0 { processed as isize } else { result }; }
        let n_u32 = (result as u32).to_le_bytes();
        let _ = write_user_buffer(unsafe { entry.add(56) }, &n_u32);
        processed += 1;
    }
    processed as isize
}

pub(in crate::syscall) fn sys_setsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *const u8,
    optlen: u32,
) -> isize {
    if crate::net::socket::is_af_alg_fd(fd) || level == 279 {
        crate::dprintln!("[af_alg] sys_setsockopt fd={} level={} opt={} len={}", fd, level, optname, optlen);
    }
    if let Err(err) = socket::validate_socket_fd(fd) {
        return socket_errno(err);
    }
    if optlen > 4096 {
        return -22;
    }
    // Per Linux: optval=NULL is only valid together with optlen=0 (e.g.
    // IP_DROP_MEMBERSHIP). A NULL optval with optlen>0 fails the kernel's
    // copy_from_sockptr() with EFAULT (setsockopt01 "invalid option buffer").
    if optval.is_null() && optlen != 0 {
        return -14; // EFAULT
    }
    let value = if optlen == 0 || optval.is_null() {
        alloc::vec::Vec::new()
    } else {
        match read_user_buffer(optval, optlen as usize) {
            Ok(value) => value,
            Err(_) => return -14,
        }
    };
    socket::setsockopt_fd(fd, level, optname, &value).map_or_else(socket_errno, |_| 0)
}

pub(in crate::syscall) fn sys_getsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *mut u8,
    optlen: *mut u32,
) -> isize {
    if let Err(err) = socket::validate_socket_fd(fd) {
        return socket_errno(err);
    }
    let len = match read_optlen(optlen) {
        Ok(len) => len,
        Err(errno) => return errno,
    };
    if optval.is_null() {
        return -14;
    }
    let (value_len, value) = match socket::getsockopt_fd(fd, level, optname) {
        Ok(value) => value,
        Err(err) => return socket_errno(err),
    };
    let copy_len = len.min(value_len);
    if write_user_buffer(optval, &value[..copy_len]).is_err() {
        return -14;
    }
    if write_user_buffer(optlen as *mut u8, &(value_len as u32).to_ne_bytes()).is_err() {
        return -14;
    }
    0
}

pub(in crate::syscall) fn sys_shutdown(fd: i32, how: i32) -> isize {
    socket::shutdown_fd(fd, how).map_or_else(socket_errno, |_| 0)
}
