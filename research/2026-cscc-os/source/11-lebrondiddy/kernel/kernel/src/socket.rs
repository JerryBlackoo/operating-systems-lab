use core::{
    cmp,
    net::SocketAddrV4,
    sync::atomic::{AtomicUsize, Ordering},
};

use alloc::{sync::Arc, vec::Vec};
use fs::INodeInterface;
use libc_types::{
    poll::PollEvent,
    types::{Stat, StatMode},
};
use lose_net_stack::connection::SocketType;
use lose_net_stack::net_trait::SocketInterface;
use polyhal::debug_console::DebugConsole;
use sync::Mutex;
use syscalls::Errno;
use vfscore::VfsResult;

use crate::syscall::NET_SERVER;

const SOCKET_IO_CHUNK: usize = 16 * 1024;
const SOCKET_QUEUE_HIGH_WATER: usize = 512 * 1024;
static SOCKET_QUEUED_BYTES: AtomicUsize = AtomicUsize::new(0);

pub fn note_socket_dequeue(bytes: usize) {
    let mut current = SOCKET_QUEUED_BYTES.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(bytes);
        match SOCKET_QUEUED_BYTES.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

pub fn socket_queue_full() -> bool {
    SOCKET_QUEUED_BYTES.load(Ordering::Relaxed) >= SOCKET_QUEUE_HIGH_WATER
}

pub fn note_socket_enqueue(bytes: usize) {
    SOCKET_QUEUED_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Debug)]
#[allow(dead_code)]
pub enum NetType {
    STEAM,
    DGRAME,
    RAW,
}

impl NetType {
    pub fn from_usize(value: usize) -> Option<Self> {
        match value {
            1 => Some(Self::STEAM),
            2 => Some(Self::DGRAME),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct SocketOptions {
    pub wsize: usize,
    pub rsize: usize,
}

#[allow(dead_code)]
pub struct Socket {
    pub domain: usize,
    pub net_type: NetType,
    pub inner: Arc<dyn SocketInterface>,
    pub options: Mutex<SocketOptions>,
    pub buf: Mutex<Vec<u8>>,
}

unsafe impl Sync for Socket {}
unsafe impl Send for Socket {}

impl Drop for Socket {
    fn drop(&mut self) {
        log::debug!("strong count: {}", Arc::strong_count(&self.inner));
        if let Ok(local) = self.inner.get_local() {
            if local.port() != 0 {
                match self.inner.get_protocol() {
                    Ok(SocketType::TCP) => NET_SERVER.remote_tcp(&local.port()),
                    Ok(SocketType::UDP) => NET_SERVER.remote_udp(&local.port()),
                    _ => {}
                }
            }
        }
    }
}

impl Socket {
    pub fn new(domain: usize, net_type: NetType) -> Arc<Self> {
        let inner: Arc<dyn SocketInterface> = match net_type {
            NetType::STEAM => NET_SERVER.blank_tcp(),
            NetType::DGRAME => NET_SERVER.blank_udp(),
            NetType::RAW => {
                panic!("can't create raw socket")
            }
        };
        Arc::new(Self {
            domain,
            net_type,
            inner,
            options: Mutex::new(SocketOptions { wsize: 0, rsize: 0 }),
            buf: Mutex::new(vec![]),
        })
    }

    pub fn recv_from(&self) -> VfsResult<(Vec<u8>, SocketAddrV4)> {
        if let Ok(local) = self.inner.get_local() {
            log::warn!("try to recv data from {}", local);
        }
        match self.inner.recv_from() {
            Ok((data, remote)) => Ok((data, remote)),
            Err(_err) => Err(Errno::EWOULDBLOCK),
        }
    }

    pub fn new_with_inner(
        domain: usize,
        net_type: NetType,
        inner: Arc<dyn SocketInterface>,
    ) -> Arc<Self> {
        Arc::new(Self {
            domain,
            net_type,
            inner,
            options: Mutex::new(SocketOptions { wsize: 0, rsize: 0 }),
            buf: Mutex::new(vec![]),
        })
    }

    pub fn reuse(&self, port: u16) -> Self {
        // NET_SERVER.get_tcp(port)
        match self.inner.get_protocol().unwrap() {
            lose_net_stack::connection::SocketType::TCP => {
                if let Some(socket_inner) = NET_SERVER.get_tcp(&port) {
                    Self {
                        domain: self.domain,
                        net_type: self.net_type,
                        inner: socket_inner,
                        options: Mutex::new(self.options.lock().clone()),
                        buf: Mutex::new(vec![]),
                    }
                } else {
                    unreachable!("can't reusetcp in blank tcp")
                }
            }
            lose_net_stack::connection::SocketType::UDP => {
                if let Some(socket_inner) = NET_SERVER.get_udp(&port) {
                    Self {
                        domain: self.domain,
                        net_type: self.net_type,
                        inner: socket_inner,
                        options: Mutex::new(self.options.lock().clone()),
                        buf: Mutex::new(vec![]),
                    }
                } else {
                    unreachable!("can't reusetcp in blank udp")
                }
            }
            lose_net_stack::connection::SocketType::RAW => todo!(),
        }
    }
}

impl INodeInterface for Socket {
    fn readat(&self, _offset: usize, buffer: &mut [u8]) -> VfsResult<usize> {
        let mut data = self.buf.lock().clone();
        // let rlen;
        // if buf.len() > 0 {
        //     rlen = cmp::min(buf.len(), buffer.len());
        //     let
        // } else {
        //     rlen = cmp::min(data.len(), buffer.len());
        //     buffer[..rlen].copy_from_slice(&data[..rlen]);
        //     self.options.lock().rsize += rlen;
        //     if rlen < data.len() {

        //     }
        // }
        // Ok(rlen)
        if data.len() == 0 {
            match self.inner.recv_from() {
                Ok((recv_data, _)) => {
                    data = recv_data;
                }
                Err(_err) => return Err(Errno::EWOULDBLOCK),
            }
        }
        let rlen = cmp::min(data.len(), buffer.len());
        buffer[..rlen].copy_from_slice(&data[..rlen]);
        note_socket_dequeue(rlen);
        self.options.lock().rsize += rlen;
        if buffer.len() == 1 {
            DebugConsole::putchar(buffer[0]);
        }
        if rlen < data.len() {
            *self.buf.lock() = data[rlen..].to_vec();
        } else {
            self.buf.lock().clear();
        }
        Ok(rlen)
    }

    fn writeat(&self, _offset: usize, buffer: &[u8]) -> VfsResult<usize> {
        if socket_queue_full() {
            return Err(Errno::EWOULDBLOCK);
        }
        let write_len = cmp::min(buffer.len(), SOCKET_IO_CHUNK);
        match self.inner.sendto(&buffer[..write_len], None) {
            Ok(len) => {
                note_socket_enqueue(len);
                self.options.lock().wsize += len;
                Ok(len)
            }
            Err(_err) => Err(Errno::EPERM),
        }
    }

    fn poll(&self, events: PollEvent) -> VfsResult<PollEvent> {
        let mut res = PollEvent::NONE;
        if events.contains(PollEvent::OUT)
            && !self.inner.is_closed().unwrap_or(true)
            && self.inner.get_remote().is_ok()
        {
            res |= PollEvent::OUT;
        }
        if self.inner.readable().unwrap_or(false) && events.contains(PollEvent::IN) {
            res |= PollEvent::IN;
        }
        Ok(res)
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.mode = StatMode::SOCKET;
        Ok(())
    }
}
