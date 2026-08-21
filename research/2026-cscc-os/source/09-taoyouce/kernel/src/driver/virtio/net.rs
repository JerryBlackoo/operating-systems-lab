//! VirtIO 网络设备驱动
//!
//! 实现 VirtIO Network Device 驱动

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::VecDeque;
use core::ptr::read_volatile;
use crate::sync::SpinLock;
use super::{VirtioDevice, VirtioMmioReg};
use super::queue::{SplitVirtqueue, QUEUE_SIZE};

/// 最大传输单元
pub const MTU: usize = 1514;

/// 接收队列索引
pub const RX_QUEUE: u16 = 0;

/// 发送队列索引
pub const TX_QUEUE: u16 = 1;

/// VirtIO 网络设备特性
pub mod features {
    pub const CSUM: u64 = 1 << 0;           // 校验和卸载
    pub const GUEST_CSUM: u64 = 1 << 1;     // Guest 校验和
    pub const MAC: u64 = 1 << 5;            // 设备有 MAC 地址
    pub const GSO: u64 = 1 << 6;            // GSO
    pub const GUEST_TSO4: u64 = 1 << 7;     // Guest TSO4
    pub const GUEST_TSO6: u64 = 1 << 8;     // Guest TSO6
    pub const GUEST_ECN: u64 = 1 << 9;      // Guest ECN
    pub const GUEST_UFO: u64 = 1 << 10;     // Guest UFO
    pub const HOST_TSO4: u64 = 1 << 11;     // Host TSO4
    pub const HOST_TSO6: u64 = 1 << 12;     // Host TSO6
    pub const HOST_ECN: u64 = 1 << 13;      // Host ECN
    pub const HOST_UFO: u64 = 1 << 14;      // Host UFO
    pub const MRG_RXBUF: u64 = 1 << 15;     // 合并接收缓冲区
    pub const STATUS: u64 = 1 << 16;        // 状态
    pub const CTRL_VQ: u64 = 1 << 17;       // 控制队列
    pub const CTRL_RX: u64 = 1 << 18;       // 控制接收
    pub const CTRL_VLAN: u64 = 1 << 19;     // 控制 VLAN
    pub const GUEST_ANNOUNCE: u64 = 1 << 21; // Guest 公告
    pub const MQ: u64 = 1 << 22;            // 多队列
}

/// VirtIO 网络包头
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtioNetHdr {
    /// 标志
    pub flags: u8,
    /// GSO 类型
    pub gso_type: u8,
    /// 头部长度
    pub hdr_len: u16,
    /// GSO 大小
    pub gso_size: u16,
    /// 校验和起始
    pub csum_start: u16,
    /// 校验和偏移
    pub csum_offset: u16,
    /// 缓冲区数量（仅当 MRG_RXBUF 启用）
    pub num_buffers: u16,
}

/// VirtIO 网络设备配置
#[repr(C)]
pub struct VirtioNetConfig {
    /// MAC 地址
    pub mac: [u8; 6],
    /// 状态
    pub status: u16,
    /// 最大队列对数
    pub max_virtqueue_pairs: u16,
    /// MTU
    pub mtu: u16,
}

/// 网络包
pub struct NetPacket {
    /// 包头
    header: Box<VirtioNetHdr>,
    /// 数据
    data: Vec<u8>,
}

impl NetPacket {
    /// 创建接收包
    pub fn new_rx() -> Self {
        Self {
            header: Box::new(VirtioNetHdr::default()),
            data: vec![0u8; MTU],
        }
    }

    /// 创建发送包
    pub fn new_tx(data: Vec<u8>) -> Self {
        Self {
            header: Box::new(VirtioNetHdr::default()),
            data,
        }
    }

    /// 获取数据
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// 获取数据长度
    pub fn len(&self) -> usize {
        self.data.len()
    }
}

/// VirtIO 网络设备
pub struct VirtioNet {
    /// 底层设备
    device: VirtioDevice,
    /// 接收队列
    rx_queue: SplitVirtqueue,
    /// 发送队列
    tx_queue: SplitVirtqueue,
    /// MAC 地址
    mac: [u8; 6],
    /// 接收缓冲区
    rx_buffers: Vec<NetPacket>,
    /// 接收队列
    rx_pending: VecDeque<NetPacket>,
    /// 发送缓冲区
    tx_buffers: Vec<NetPacket>,
}

impl VirtioNet {
    /// 创建网络设备
    pub fn new(device: VirtioDevice) -> Result<Self, &'static str> {
        // 读取 MAC 地址
        let config_addr = device.config_addr();
        // PCI transport may legitimately expose no device-specific config
        // capability (`config_addr == 0`). Release builds previously performed
        // a volatile read from NULL (UB) and happened to fail later with
        // InvalidParam; debug assertions correctly panic at boot. Reject before
        // touching MMIO so the caller can use the existing fallback network.
        if config_addr == 0 {
            return Err("network device config unavailable");
        }
        let mut mac = [0u8; 6];
        for i in 0..6 {
            mac[i] = unsafe { read_volatile((config_addr + i) as *const u8) };
        }

        // 创建队列
        let rx_queue = SplitVirtqueue::new(RX_QUEUE);
        let tx_queue = SplitVirtqueue::new(TX_QUEUE);

        // 配置接收队列
        device.write_reg(VirtioMmioReg::QueueSel, RX_QUEUE as u32);
        let queue_max = device.read_reg(VirtioMmioReg::QueueNumMax);
        if queue_max == 0 {
            return Err("RX queue not available");
        }
        device.write_reg(VirtioMmioReg::QueueNum, rx_queue.size() as u32);

        let desc_addr = rx_queue.desc_addr();
        let avail_addr = rx_queue.avail_addr();
        let used_addr = rx_queue.used_addr();

        device.write_reg(VirtioMmioReg::QueueDescLow, desc_addr as u32);
        device.write_reg(VirtioMmioReg::QueueDescHigh, (desc_addr >> 32) as u32);
        device.write_reg(VirtioMmioReg::QueueDriverLow, avail_addr as u32);
        device.write_reg(VirtioMmioReg::QueueDriverHigh, (avail_addr >> 32) as u32);
        device.write_reg(VirtioMmioReg::QueueDeviceLow, used_addr as u32);
        device.write_reg(VirtioMmioReg::QueueDeviceHigh, (used_addr >> 32) as u32);
        device.write_reg(VirtioMmioReg::QueueReady, 1);

        // 配置发送队列
        device.write_reg(VirtioMmioReg::QueueSel, TX_QUEUE as u32);
        let queue_max = device.read_reg(VirtioMmioReg::QueueNumMax);
        if queue_max == 0 {
            return Err("TX queue not available");
        }
        device.write_reg(VirtioMmioReg::QueueNum, tx_queue.size() as u32);

        let desc_addr = tx_queue.desc_addr();
        let avail_addr = tx_queue.avail_addr();
        let used_addr = tx_queue.used_addr();

        device.write_reg(VirtioMmioReg::QueueDescLow, desc_addr as u32);
        device.write_reg(VirtioMmioReg::QueueDescHigh, (desc_addr >> 32) as u32);
        device.write_reg(VirtioMmioReg::QueueDriverLow, avail_addr as u32);
        device.write_reg(VirtioMmioReg::QueueDriverHigh, (avail_addr >> 32) as u32);
        device.write_reg(VirtioMmioReg::QueueDeviceLow, used_addr as u32);
        device.write_reg(VirtioMmioReg::QueueDeviceHigh, (used_addr >> 32) as u32);
        device.write_reg(VirtioMmioReg::QueueReady, 1);

        // 完成初始化
        device.finish_init();

        crate::println!(
            "[virtio-net] Network device initialized, MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        let mut net = Self {
            device,
            rx_queue,
            tx_queue,
            mac,
            rx_buffers: Vec::new(),
            rx_pending: VecDeque::new(),
            tx_buffers: Vec::new(),
        };

        // 填充接收缓冲区
        net.fill_rx_buffers();

        Ok(net)
    }

    /// 获取 MAC 地址
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// 填充接收缓冲区
    fn fill_rx_buffers(&mut self) {
        for _ in 0..QUEUE_SIZE / 2 {
            let packet = NetPacket::new_rx();
            let header_addr = packet.header.as_ref() as *const _ as u64;
            let data_addr = packet.data.as_ptr() as u64;

            let bufs = vec![
                (header_addr, core::mem::size_of::<VirtioNetHdr>() as u32, true),
                (data_addr, MTU as u32, true),
            ];

            if self.rx_queue.add_buf(&bufs).is_some() {
                self.rx_buffers.push(packet);
            } else {
                break;
            }
        }

        // 通知设备
        self.device.notify(RX_QUEUE as u32);
    }

    /// 发送数据包
    pub fn send(&mut self, data: Vec<u8>) -> Result<(), &'static str> {
        if data.len() > MTU {
            return Err("Packet too large");
        }

        let packet = NetPacket::new_tx(data);
        let header_addr = packet.header.as_ref() as *const _ as u64;
        let data_addr = packet.data.as_ptr() as u64;

        let bufs = vec![
            (header_addr, core::mem::size_of::<VirtioNetHdr>() as u32, false),
            (data_addr, packet.data.len() as u32, false),
        ];

        self.tx_queue.add_buf(&bufs).ok_or("TX queue full")?;
        self.tx_buffers.push(packet);

        // 通知设备
        self.device.notify(TX_QUEUE as u32);

        Ok(())
    }

    /// 接收数据包
    pub fn recv(&mut self) -> Option<Vec<u8>> {
        // 处理已完成的接收
        while let Some((idx, len)) = self.rx_queue.pop_used() {
            self.rx_queue.free_chain(idx);

            if let Some(packet) = self.rx_buffers.get(idx as usize) {
                let data_len = len as usize - core::mem::size_of::<VirtioNetHdr>();
                let data = packet.data[..data_len].to_vec();
                self.rx_pending.push_back(NetPacket::new_tx(data));
            }
        }

        // 返回一个接收到的包
        self.rx_pending.pop_front().map(|p| p.data)
    }

    /// 处理中断
    pub fn handle_interrupt(&mut self) {
        self.device.ack_interrupt();

        // 处理发送完成
        while let Some((idx, _)) = self.tx_queue.pop_used() {
            self.tx_queue.free_chain(idx);
        }

        // 处理接收
        self.recv();

        // 补充接收缓冲区
        self.fill_rx_buffers();
    }
}

/// 全局网络设备
static NET_DEVICE: SpinLock<Option<VirtioNet>> = SpinLock::new(None);

/// 全局 PCI 网络设备（LoongArch64 使用 virtio-drivers crate）
#[cfg(target_arch = "loongarch64")]
type PciNetDevice = virtio_drivers::device::net::VirtIONet<super::hal_impl::VirtioHalImpl, virtio_drivers::transport::pci::PciTransport, 16>;

#[cfg(target_arch = "loongarch64")]
static PCI_NET_DEVICE: SpinLock<Option<PciNetDevice>> = SpinLock::new(None);

/// 初始化 PCI 网络设备（LoongArch64 专用）
///
/// QEMU LA virt 的 virtio-net PCI capability 会让 upstream `VirtIONet::new`
/// 返回 InvalidParam，且其 Err 析构路径会向堆释放一个重叠/双释放块
/// （AVL 防御检查实锤：`[HEAPDBG] overlap with successor ... outer_ra` 落在
/// `driver::init → virtio::init → pci::init`）。该路径从未成功过，正式网络
/// 一直走软件 fallback；因此 LA 上直接跳过外部 crate 的 PCI net 构造，
/// 与 debug 构建同策略，避免污染内核堆。
#[cfg(target_arch = "loongarch64")]
pub fn init_pci_device(transport: virtio_drivers::transport::pci::PciTransport) {
    let _ = transport;
    crate::println!("[virtio-net-pci] skip unsupported PCI net, use fallback");
}

/// 初始化网络设备
pub fn init_device(device: VirtioDevice) {
    match VirtioNet::new(device) {
        Ok(net) => {
            *NET_DEVICE.lock() = Some(net);
            crate::println!("[virtio-net] Network device ready");
        }
        Err(e) => {
            crate::println!("[virtio-net] Failed to initialize: {}", e);
        }
    }
}

/// 发送数据包
pub fn send_packet(data: Vec<u8>) -> Result<(), &'static str> {
    #[cfg(target_arch = "loongarch64")]
    {
        if let Some(net) = PCI_NET_DEVICE.lock().as_mut() {
            let tx_buf = virtio_drivers::device::net::TxBuffer::from(data.as_slice());
            return net.send(tx_buf).map_err(|_| "PCI network send failed");
        }
    }

    NET_DEVICE.lock()
        .as_mut()
        .ok_or("Network device not initialized")?
        .send(data)
}

/// 接收数据包
pub fn recv_packet() -> Option<Vec<u8>> {
    #[cfg(target_arch = "loongarch64")]
    {
        if let Some(net) = PCI_NET_DEVICE.lock().as_mut() {
            let rx_buf = net.receive().ok()?;
            let data = rx_buf.packet().to_vec();
            let _ = net.recycle_rx_buffer(rx_buf);
            return Some(data);
        }
    }

    NET_DEVICE.lock()
        .as_mut()
        .and_then(|net| net.recv())
}

/// 获取 MAC 地址
pub fn get_mac() -> Option<[u8; 6]> {
    #[cfg(target_arch = "loongarch64")]
    {
        if let Some(net) = PCI_NET_DEVICE.lock().as_ref() {
            return Some(net.mac_address());
        }
    }

    NET_DEVICE.lock()
        .as_ref()
        .map(|net| net.mac())
}

/// VirtIO 网络设备自测试
pub fn self_test() {
    crate::println!("[virtio-net] Running self-test...");

    // 测试常量
    let _mtu = MTU;
    let _rx_queue = RX_QUEUE;
    let _tx_queue = TX_QUEUE;
    crate::println!("[virtio-net] MTU={} RX_QUEUE={} TX_QUEUE={}", MTU, RX_QUEUE, TX_QUEUE);

    // 测试 features 模块
    let feature_flags = [
        features::CSUM,
        features::GUEST_CSUM,
        features::MAC,
        features::GSO,
        features::GUEST_TSO4,
        features::GUEST_TSO6,
        features::GUEST_ECN,
        features::GUEST_UFO,
        features::HOST_TSO4,
        features::HOST_TSO6,
        features::HOST_ECN,
        features::HOST_UFO,
        features::MRG_RXBUF,
        features::STATUS,
        features::CTRL_VQ,
        features::CTRL_RX,
        features::CTRL_VLAN,
        features::GUEST_ANNOUNCE,
        features::MQ,
    ];
    for f in &feature_flags {
        let _flag = *f;
    }
    crate::println!("[virtio-net] Feature flags tested");

    // 测试 VirtioNetHdr
    let hdr = VirtioNetHdr {
        flags: 0,
        gso_type: 0,
        hdr_len: 0,
        gso_size: 0,
        csum_start: 0,
        csum_offset: 0,
        num_buffers: 0,
    };
    crate::println!("[virtio-net] VirtioNetHdr: flags={} gso_type={}", hdr.flags, hdr.gso_type);

    // 测试 VirtioNetConfig
    let config = VirtioNetConfig {
        mac: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
        status: 1,
        max_virtqueue_pairs: 1,
        mtu: 1500,
    };
    crate::println!("[virtio-net] Config: MAC={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} mtu={}",
        config.mac[0], config.mac[1], config.mac[2], config.mac[3], config.mac[4], config.mac[5],
        config.mtu);

    // 测试 NetPacket
    let rx_packet = NetPacket::new_rx();
    crate::println!("[virtio-net] RX packet len: {}", rx_packet.len());
    let _data = rx_packet.data();

    let tx_data = vec![0u8; 64];
    let tx_packet = NetPacket::new_tx(tx_data);
    crate::println!("[virtio-net] TX packet len: {}", tx_packet.len());

    // 测试全局函数（不实际执行，因为没有设备）
    let _send_result = send_packet(vec![0u8; 64]);
    let _recv_result = recv_packet();
    let _mac = get_mac();

    crate::println!("[virtio-net] Self-test passed!");
}
