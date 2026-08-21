//! 网络协议栈
//!
//! 实现 TCP/IP 协议栈和 Socket API

pub mod backend;
pub mod bufpool;
pub mod af_alg;
pub mod inet;
pub mod loopback;
pub mod smol_device;
pub mod net_stack;
pub mod dns;
pub mod socket;
pub mod zerocopy;
pub mod procnet;
pub mod netioctl;
pub mod tcp;
pub mod tcp_cong;
pub mod tcp_retransmit;
pub mod udp;
pub mod icmp;
pub mod multicast;
pub mod af_netlink;
pub mod af_packet;

/// MAC 地址
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xFF; 6]);

    pub fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }
}

/// IPv4 地址
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ipv4Addr(pub u32);

impl Ipv4Addr {
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr(0);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr(0xFFFFFFFF);
    pub const LOCALHOST: Ipv4Addr = Ipv4Addr(0x7F000001);

    pub fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self(((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32))
    }

    pub fn octets(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }

    pub fn from_bytes(bytes: [u8; 4]) -> Self {
        Self(u32::from_be_bytes(bytes))
    }
}

/// 端口号
pub type Port = u16;

/// Socket 地址
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAddr {
    pub ip: Ipv4Addr,
    pub port: Port,
}

impl SocketAddr {
    pub fn new(ip: Ipv4Addr, port: Port) -> Self {
        Self { ip, port }
    }
}

/// 网络模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[net] Running self-test...");

    // 测试 MAC 地址
    let mac = MacAddr::new([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
    assert_eq!(mac.0[0], 0x00);
    let _ = MacAddr::BROADCAST;

    // 测试 IPv4 地址
    let ip = Ipv4Addr::new(192, 168, 1, 1);
    let octets = ip.octets();
    assert_eq!(octets[0], 192);
    let ip2 = Ipv4Addr::from_bytes([10, 0, 0, 1]);
    assert_eq!(ip2.0, 0x0A000001);
    let _ = Ipv4Addr::UNSPECIFIED;
    let _ = Ipv4Addr::BROADCAST;
    let _ = Ipv4Addr::LOCALHOST;

    // 测试 Socket 地址
    let addr = SocketAddr::new(ip, 8080);
    assert_eq!(addr.port, 8080);

    // ========== 测试 Socket API ==========
    test_socket_api();

    // ========== 测试零拷贝 ==========
    test_zerocopy();

    crate::println!("[net] Self-test passed!");
    Ok(())
}

/// 测试 Socket API
fn test_socket_api() {
    socket::init();

    // 测试 SocketError 枚举
    let errors = [
        socket::SocketError::InvalidArg,
        socket::SocketError::AddrInUse,
        socket::SocketError::ConnectionRefused,
        socket::SocketError::ConnectionReset,
        socket::SocketError::NotConnected,
        socket::SocketError::AlreadyConnected,
        socket::SocketError::WouldBlock,
        socket::SocketError::NoResources,
        socket::SocketError::BadFd,
        socket::SocketError::Timeout,
    ];
    for e in &errors {
        crate::println!("[net/socket] Error: {:?}", e);
    }
}

/// 测试零拷贝
fn test_zerocopy() {
    use crate::mm::PhysPageNum;

    zerocopy::init();

    // 测试 SgEntry 和 SgList
    let entry = zerocopy::SgEntry::new(PhysPageNum(0x1000), 512, 0);
    let mut sg_list = zerocopy::SgList::new();
    let _ = sg_list.add(entry);
    let _ = sg_list.entries();
    let _ = sg_list.total_len();
    let _ = sg_list.is_empty();

    // 测试 ZeroCopyBuffer
    if let Ok(mut zc_buf) = zerocopy::ZeroCopyBuffer::alloc(4096) {
        zc_buf.inc_ref();
        let _ = zc_buf.ref_count();
        let _ = zc_buf.dec_ref();
        zc_buf.complete();
        let _ = zc_buf.is_completed();
        zc_buf.set_data_len(1024);
        zc_buf.set_head_offset(64);
        let _ = zc_buf.to_sg_list();
    }

    // 测试 ZeroCopySocket
    let zc_socket = zerocopy::ZeroCopySocket::new(8080);
    let _ = zc_socket.prealloc_recv_buffers(4);
    zc_socket.set_zc_enabled(true);
    let _ = zc_socket.recv_zc();
    zc_socket.handle_recv_complete(PhysPageNum(0x2000), 512, 0x0A000001, 80);

    if let Ok(buf) = zerocopy::ZeroCopyBuffer::alloc(1024) {
        let arc_buf = alloc::sync::Arc::new(buf);
        let _ = zc_socket.send_zc(arc_buf, 0x0A000001, 80);
    }

    // 测试 ZcSendRequest 和 ZcRecvRequest
    if let Ok(buf) = zerocopy::ZeroCopyBuffer::alloc(512) {
        let arc_buf = alloc::sync::Arc::new(buf);
        let send_req = zerocopy::ZcSendRequest::new(arc_buf.clone(), 0x0A000001, 80);
        let send_req = send_req.on_complete(|_result| {});
        send_req.complete(Ok(512));

        let mut recv_req = zerocopy::ZcRecvRequest::new(arc_buf.clone());
        recv_req.set_recv_info(0x0A000001, 80, 256);
        let _ = recv_req.buffer();
    }

    // 测试 PagePool
    if let Ok(pool) = zerocopy::PagePool::new(8) {
        if let Some(page) = pool.alloc() {
            pool.free(page);
        }
        let _ = pool.stats();
    }

    // 测试全局页面分配
    if let Some(page) = zerocopy::alloc_page() {
        zerocopy::free_page(page);
    }
}
