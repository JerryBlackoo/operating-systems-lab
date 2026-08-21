use super::SysResult;
use crate::socket::{self, NetType};
use crate::user::socket_pair::create_socket_pair;
use crate::user::UserTaskContainer;
use crate::utils::useref::UserRef;
use alloc::sync::Arc;
use core::cmp;
use core::net::{Ipv4Addr, SocketAddrV4};
use devices::get_net_device;
use executor::yield_now;
use fs::file::File;
use libc_types::fcntl::OpenFlags;
use log::{debug, warn};
use lose_net_stack::connection::NetServer;
use lose_net_stack::net_trait::NetInterface;
use lose_net_stack::results::NetServerError;
use lose_net_stack::MacAddress;
use polyhal::{MappingFlags, VirtAddr};
use sync::Lazy;
use syscalls::Errno;

type Socket = socket::Socket;
const SOCKET_IO_CHUNK: usize = 16 * 1024;
const SOL_SOCKET: usize = 1;
const IPPROTO_IP: usize = 0;
const IPPROTO_TCP: usize = 6;
const IPPROTO_UDP: usize = 17;
const SO_REUSEADDR: usize = 0x2;
const SO_ERROR: usize = 0x4;
const SO_SNDBUF: usize = 0x7;
const SO_RCVBUF: usize = 0x8;

#[derive(Debug)]
pub struct NetMod;

impl NetInterface for NetMod {
    fn send(data: &[u8]) {
        get_net_device(0)
            .send(data)
            .expect("can't send data in net interface");
    }

    fn local_mac_address() -> MacAddress {
        MacAddress::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56])
    }
}

pub static NET_SERVER: Lazy<Arc<NetServer<NetMod>>> = Lazy::new(|| {
    Arc::new(NetServer::new(
        MacAddress::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
        Ipv4Addr::new(10, 0, 2, 15),
    ))
});

#[derive(Debug)]
#[allow(dead_code)]
struct SocketAddr {
    sa_family: u16,
    sa_data: [u8; 14],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SocketAddrIn {
    family: u16,
    in_port: u16,
    addr: Ipv4Addr,
    sin_zero: [u8; 8],
}

impl UserTaskContainer {
    fn socket_killed_result(&self) -> Option<SysResult> {
        self.task.exit_code().is_some().then_some(Err(Errno::EINTR))
    }

    fn check_user_writable_u32(&self, ptr: UserRef<u32>) -> Result<(), Errno> {
        if ptr.is_null() {
            return Err(Errno::EFAULT);
        }
        for offset in 0..core::mem::size_of::<u32>() {
            let Some((_, flags)) = self
                .task
                .page_table
                .translate(VirtAddr::from(ptr.addr().saturating_add(offset)))
            else {
                return Err(Errno::EFAULT);
            };
            super::ensure_user_mapping(flags)?;
            if !flags.contains(MappingFlags::W) {
                return Err(Errno::EFAULT);
            }
        }
        Ok(())
    }

    pub fn sys_socket(&self, domain: usize, net_type: usize, protocol: usize) -> SysResult {
        debug!(
            "[task {}] sys_socket @ domain: {:#x}, net_type: {:#x}, protocol: {:#x}",
            self.tid, domain, net_type, protocol
        );
        let fd = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        log::debug!(
            "net_type: {:?}",
            NetType::from_usize(net_type).ok_or(Errno::EINVAL)?
        );
        let net_type = NetType::from_usize(net_type).ok_or(Errno::EINVAL)?;
        let socket = Socket::new(domain, net_type);
        self.task.set_fd(fd, File::new_dev(socket));
        Ok(fd)
    }

    pub fn sys_socket_pair(
        &self,
        domain: usize,
        net_type: usize,
        protocol: usize,
        socket_vector: UserRef<u32>,
    ) -> SysResult {
        debug!(
            "sys_socket_pair @ domain: {} net_type: {:#x} protocol: {} socket_vector: {:?}",
            domain, net_type, protocol, socket_vector
        );
        let fds = socket_vector.slice_mut_with_len(2);

        let socket = create_socket_pair();
        let rx_fd = self.task.alloc_fd().ok_or(Errno::ENFILE)?;
        self.task.set_fd(rx_fd, File::new_dev(socket.clone()));
        fds[0] = rx_fd as u32;

        let tx_fd = self.task.alloc_fd().ok_or(Errno::ENFILE)?;
        self.task.set_fd(tx_fd, File::new_dev(socket.clone()));
        fds[1] = tx_fd as u32;

        Ok(0)
    }

    pub fn sys_bind(
        &self,
        socket_fd: usize,
        addr_ptr: UserRef<SocketAddrIn>,
        address_len: usize,
    ) -> SysResult {
        debug!(
            "[task {}] sys_bind @ socket: {:#x}, addr_ptr: {}, address_len: {:#x}",
            self.tid, socket_fd, addr_ptr, address_len
        );
        let socket_addr = self.read_user_value(addr_ptr)?;
        debug!("try to bind {:?} to socket {}", socket_addr, socket_fd);
        let socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EINVAL)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;

        let net_server = NET_SERVER.clone();
        let port = socket_addr.in_port.to_be();
        debug!("read port {}", port);

        if socket_addr.family != 0x02 {
            warn!("only support IPV4 now");
            return Err(Errno::EAFNOSUPPORT);
        }

        match socket.net_type {
            NetType::STEAM => {
                if net_server.tcp_is_used(port) {
                    return Err(Errno::EADDRINUSE);
                }
            }
            NetType::DGRAME => {
                if net_server.udp_is_used(port) {
                    return Err(Errno::EADDRINUSE);
                }
            }
            NetType::RAW => {}
        }

        let local = SocketAddrV4::new(socket_addr.addr, port);
        socket
            .inner
            .clone()
            .bind(local)
            .map_err(|_| Errno::EALREADY)?;
        debug!("socket_addr: {:#x?}", socket_addr);
        Ok(0)
    }

    pub fn sys_listen(&self, socket_fd: usize, backlog: usize) -> SysResult {
        debug!(
            "[task {}] sys_listen @ socket_fd: {:#x}, backlog: {:#x}",
            self.tid, socket_fd, backlog
        );
        let _ = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EINVAL)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?
            .inner
            .clone()
            .listen();
        Ok(0)
    }

    pub async fn sys_accept(&self, socket_fd: usize, socket_addr: usize, len: usize) -> SysResult {
        debug!(
            "[task {}] sys_accept @ socket_fd: {:#x}, socket_addr: {:#x}, len: {:#x}",
            self.tid, socket_fd, socket_addr, len
        );
        let file = self.task.get_fd(socket_fd).ok_or(Errno::EINVAL)?;
        let socket = file
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;
        debug!("flags: {:?}", file.flags.lock());
        let fd = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        loop {
            if let Some(result) = self.socket_killed_result() {
                return result;
            }
            if let Ok(new_socket) = socket.inner.accept() {
                self.task.set_fd(
                    fd,
                    File::new_dev(Socket::new_with_inner(
                        socket.domain,
                        socket.net_type,
                        new_socket,
                    )),
                );
                break;
            }

            if !self.task.tcb.read().signal.is_empty(None) {
                return Err(Errno::EINTR);
            }

            yield_now().await;
        }
        Ok(fd)
    }

    pub async fn sys_connect(
        &self,
        socket_fd: usize,
        socket_addr: UserRef<SocketAddrIn>,
        len: usize,
    ) -> SysResult {
        warn!(
            "[task {}] sys_connect @ socket_fd: {:#x}, socket_addr: {:#x?}, len: {:#x}",
            self.tid, socket_fd, socket_addr, len
        );
        let socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EINVAL)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;

        let socket_addr = self.read_user_value(socket_addr)?;
        let remote = SocketAddrV4::new(socket_addr.addr, socket_addr.in_port.to_be());
        if let Some(result) = self.socket_killed_result() {
            return result;
        }
        let local_ip = socket
            .inner
            .get_local()
            .map(|addr| *addr.ip())
            .unwrap_or_else(|_| NET_SERVER.get_local_ip());
        if socket.net_type == NetType::STEAM
            && !remote.ip().is_loopback()
            && !remote.ip().is_unspecified()
            && *remote.ip() != local_ip
        {
            return Err(Errno::ENETUNREACH);
        }
        socket
            .inner
            .clone()
            .connect(remote)
            .map(|_| 0)
            .map_err(|err| match err {
                NetServerError::Blocking => Errno::ECONNREFUSED,
                _ => Errno::EINVAL,
            })
    }

    pub async fn sys_recvfrom(
        &self,
        socket_fd: usize,
        buffer_ptr: UserRef<u8>,
        len: usize,
        flags: usize,
        addr: UserRef<SocketAddrIn>,
        addr_len: UserRef<usize>,
    ) -> SysResult {
        debug!(
            "[task {}] sys_recvfrom @ socket_fd: {:#x}, buffer_ptr: {}, len: {:#x}, flags: {:#x}, addr: {:#x?}, addr_len: {:#x?}", 
            self.tid, socket_fd, buffer_ptr, len, flags, addr, addr_len
        );
        let buffer = buffer_ptr.slice_mut_with_len(cmp::min(len, SOCKET_IO_CHUNK));
        let file = self.task.get_fd(socket_fd).ok_or(Errno::EINVAL)?;
        let socket = file
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;

        let (data, remote) = loop {
            if let Some(result) = self.socket_killed_result() {
                return result;
            }
            let res = socket.recv_from();

            match res {
                Ok(r) => break r,
                Err(_) => {
                    if file.flags.lock().contains(OpenFlags::NONBLOCK) {
                        return Err(Errno::EAGAIN);
                    }
                    yield_now().await
                }
            }
        };
        let rlen = cmp::min(data.len(), buffer.len());
        buffer[..rlen].copy_from_slice(&data[..rlen]);
        socket::note_socket_dequeue(data.len());

        if addr.is_valid() {
            addr.with_mut(|socket_addr| {
                socket_addr.in_port = remote.port().to_be();
                socket_addr.family = 2;
                socket_addr.addr = *remote.ip();
            });
        }
        Ok(rlen)
    }

    pub fn sys_getsockname(
        &self,
        socket_fd: usize,
        addr_ptr: UserRef<SocketAddrIn>,
        len_ptr: UserRef<u32>,
    ) -> SysResult {
        debug!(
            "sys_getsockname @ socket_fd: {:#x}, addr_ptr: {}, len_ptr: {}",
            socket_fd, addr_ptr, len_ptr
        );
        let len = self.read_user_value(len_ptr)?;
        if len as usize > isize::MAX as usize {
            return Err(Errno::EINVAL);
        }
        let socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EBADF)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::ENOTSOCK)?;
        let socket_address = socket.inner.get_local().map_err(|_| Errno::ENOTCONN)?;
        if len as usize >= core::mem::size_of::<SocketAddrIn>() {
            let socket_addr = SocketAddrIn {
                family: 2,
                in_port: socket_address.port().to_be(),
                addr: *socket_address.ip(),
                sin_zero: [0; 8],
            };
            self.write_user_value(addr_ptr, &socket_addr)?;
        }
        self.write_user_value(len_ptr, &(core::mem::size_of::<SocketAddrIn>() as u32))?;
        debug!("socket address: {:?}", socket_address);
        Ok(0)
    }

    pub fn sys_getpeername(
        &self,
        socket_fd: usize,
        addr_ptr: UserRef<SocketAddrIn>,
        len_ptr: UserRef<u32>,
    ) -> SysResult {
        debug!(
            "[task {}] sys_getpeername @ socket_fd: {:#x}, addr_ptr: {}, len_ptr: {}",
            self.tid, socket_fd, addr_ptr, len_ptr
        );
        let len = self.read_user_value(len_ptr)?;
        if len as usize > isize::MAX as usize {
            return Err(Errno::EINVAL);
        }
        let socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EBADF)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::ENOTSOCK)?;
        let socket_address = socket.inner.get_remote().map_err(|_| Errno::ENOTCONN)?;
        if len as usize >= core::mem::size_of::<SocketAddrIn>() {
            let socket_addr = SocketAddrIn {
                family: 2,
                in_port: socket_address.port().to_be(),
                addr: *socket_address.ip(),
                sin_zero: [0; 8],
            };
            self.write_user_value(addr_ptr, &socket_addr)?;
        }
        self.write_user_value(len_ptr, &(core::mem::size_of::<SocketAddrIn>() as u32))?;
        debug!("[task {}] socket address: {:?}", self.tid, socket_address);
        Ok(0)
    }

    pub fn sys_setsockopt(
        &self,
        socket: usize,
        level: usize,
        optname: usize,
        optval: usize,
        optlen: usize,
    ) -> SysResult {
        log::warn!("[task {}]sys_setsockopt @ socket: {:#x}, level: {:#x}, optname: {:#x}, optval: {:#x}, optlen: {:#x}", self.tid, socket, level, optname, optval, optlen);
        // Ok(0)但在网络游戏这种实时通信中，这种减少包的做法，如果网络较差的时候，可能会引起比较大的波动，比如玩家正在PK，发了技能没有很快的反馈，过一会儿很多技能效果一起回来，这个体验是比较差的。

        // 0x1a SO_ATTACH_FILTER
        // match optname {
        //     0x1 | 0x2 | 0x1a => Ok(0),
        //     _ => {
        //         Err(LinuxError::EPERM)
        //     }
        // }
        Ok(0)
    }

    pub fn sys_getsockopt(
        &self,
        socket_fd: usize,
        level: usize,
        optname: usize,
        optval: UserRef<u32>,
        optlen: UserRef<u32>,
    ) -> SysResult {
        debug!("[task {}] sys_getsockopt @ socket: {:#x}, level: {:#x}, optname: {:#x}, optval: {:#x?}, optlen: {:#x?}", 
        self.tid, socket_fd, level, optname, optval, optlen);
        let _socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EBADF)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::ENOTSOCK)?;
        let len = self.read_user_value(optlen)?;
        if len < core::mem::size_of::<u32>() as u32 || len > 4096 {
            return Err(Errno::EINVAL);
        }
        self.check_user_writable_u32(optval)?;

        match level {
            SOL_SOCKET => match optname {
                SO_SNDBUF | SO_RCVBUF | SO_REUSEADDR => self.write_user_value(optval, &32000)?,
                SO_ERROR => return Err(Errno::EPERM),
                _ => return Err(Errno::EOPNOTSUPP),
            },
            IPPROTO_IP | IPPROTO_TCP => return Err(Errno::ENOPROTOOPT),
            IPPROTO_UDP => return Err(Errno::EOPNOTSUPP),
            _ => {
                return Err(Errno::EOPNOTSUPP);
            }
        }
        self.write_user_value(optlen, &(core::mem::size_of::<u32>() as u32))?;
        // debug!("ptr value: {:?}", optval);
        Ok(0)
    }

    pub async fn sys_sendto(
        &self,
        socket_fd: usize,
        buffer_ptr: UserRef<u8>,
        len: usize,
        flags: usize,
        addr_ptr: UserRef<SocketAddrIn>,
        _address_len: usize,
    ) -> SysResult {
        debug!(
            "[task {}] sys_send @ socket_fd: {:#x}, buffer_ptr: {}, len: {:#x}, flags: {:#x}",
            self.tid, socket_fd, buffer_ptr, len, flags
        );
        let buffer = buffer_ptr.slice_mut_with_len(cmp::min(len, SOCKET_IO_CHUNK));
        let socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EINVAL)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;

        debug!("send");

        if socket
            .inner
            .get_local()
            .map_err(|_| Errno::ENOTCONN)?
            .port()
            == 0
        {
            socket
                .inner
                .clone()
                .bind(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0))
                .map_err(|_| Errno::EALREADY)?;
        }

        let remote = if addr_ptr.is_valid() {
            let socket_addr = self.read_user_value(addr_ptr)?;
            Some(SocketAddrV4::new(
                socket_addr.addr,
                socket_addr.in_port.to_be(),
            ))
        } else {
            None
        };

        while socket::socket_queue_full() {
            if let Some(result) = self.socket_killed_result() {
                return result;
            }
            yield_now().await;
        }
        let wlen = socket
            .inner
            .sendto(buffer, remote)
            .map_err(|_| Errno::EIO)?;
        socket::note_socket_enqueue(wlen);
        Ok(wlen)
    }

    pub fn sys_shutdown(&self, socket_fd: usize, how: usize) -> SysResult {
        debug!(
            "[task {}] sys_shutdown socket_fd: {:#x}, how: {:#x}",
            self.tid, socket_fd, how
        );
        let _socket = self
            .task
            .get_fd(socket_fd)
            .ok_or(Errno::EINVAL)?
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;
        Ok(0)
    }

    pub async fn sys_accept4(
        &self,
        socket_fd: usize,
        socket_addr: UserRef<SocketAddrIn>,
        len: usize,
        flags: usize,
    ) -> SysResult {
        let flags = OpenFlags::from_bits_truncate(flags);
        log::info!(
            "[task {}] sys_accept4 @ socket_fd: {:#x}, socket_addr: {:#x?}, len: {:#x}, flags: {:?}",
            self.tid,
            socket_fd,
            socket_addr,
            len,
            flags
        );
        let file = self.task.get_fd(socket_fd).ok_or(Errno::EINVAL)?;
        let socket = file
            .get_bare_file()
            .downcast_arc::<Socket>()
            .map_err(|_| Errno::EINVAL)?;
        let fd = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        loop {
            if let Some(result) = self.socket_killed_result() {
                return result;
            }
            if let Ok(new_socket) = socket.inner.accept() {
                socket_addr.with_mut(|sa| {
                    sa.family = 2;
                    sa.in_port = new_socket.get_remote().unwrap().port();
                    sa.addr = new_socket.get_remote().unwrap().ip().clone();
                });
                let new_file = File::new_dev(Socket::new_with_inner(
                    socket.domain,
                    socket.net_type,
                    new_socket,
                ));
                *new_file.flags.lock() = flags;
                self.task.set_fd(fd, new_file);
                break Ok(fd);
            } else if file.flags.lock().contains(OpenFlags::NONBLOCK) {
                break Err(Errno::EAGAIN);
            }
            yield_now().await;
        }
    }
}
