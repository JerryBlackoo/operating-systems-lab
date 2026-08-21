//! AF_PACKET protocol family implementation — per Linux net/packet/af_packet.c
//!
//! Packet sockets provide raw access to network device frames. Used by tcpdump,
//! dhclient, arp tools, and other low-level networking utilities.
//!
//! Reference: https://github.com/torvalds/linux/blob/master/net/packet/af_packet.c
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/if_packet.h
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/if_ether.h

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::socket::{SocketError, SocketFile};

// ============================================================================
// Packet socket types — per include/uapi/linux/if_packet.h
// ============================================================================

/// struct sockaddr_ll fields
pub const PACKET_HOST: u8         = 0;   // To us
pub const PACKET_BROADCAST: u8    = 1;   // To all
pub const PACKET_MULTICAST: u8    = 2;   // To group
pub const PACKET_OTHERHOST: u8    = 3;   // To someone else
pub const PACKET_OUTGOING: u8     = 4;   // Originated by us
pub const PACKET_LOOPBACK: u8     = 5;   // MC/BRD frame looped back

// ============================================================================
// SOL_PACKET options — per include/uapi/linux/if_packet.h
// ============================================================================

pub const SOL_PACKET: i32 = 263;

pub const PACKET_ADD_MEMBERSHIP: i32   = 1;
pub const PACKET_DROP_MEMBERSHIP: i32  = 2;
pub const PACKET_RECV_OUTPUT: i32      = 3;
pub const PACKET_RX_RING: i32         = 5;
pub const PACKET_STATISTICS: i32      = 6;
pub const PACKET_COPY_THRESH: i32     = 7;
pub const PACKET_AUXDATA: i32         = 8;
pub const PACKET_ORIGDEV: i32         = 9;
pub const PACKET_VERSION: i32         = 10;
pub const PACKET_HDRLEN: i32          = 11;
pub const PACKET_RESERVE: i32         = 12;
pub const PACKET_TX_RING: i32         = 13;
pub const PACKET_LOSS: i32            = 14;
pub const PACKET_VNET_HDR: i32        = 15;
pub const PACKET_TX_TIMESTAMP: i32    = 16;
pub const PACKET_TIMESTAMP: i32       = 17;
pub const PACKET_FANOUT: i32          = 18;
pub const PACKET_TX_HAS_OFF: i32      = 19;
pub const PACKET_QDISC_BYPASS: i32    = 20;
pub const PACKET_ROLLOVER_STATS: i32  = 21;
pub const PACKET_FANOUT_DATA: i32     = 22;
pub const PACKET_IGNORE_OUTGOING: i32 = 23;

// TPACKET versions
pub const TPACKET_V1: i32 = 0;
pub const TPACKET_V2: i32 = 1;
pub const TPACKET_V3: i32 = 2;

// ============================================================================
// Ethernet constants — per include/uapi/linux/if_ether.h
// ============================================================================

pub const ETH_P_ALL: u16      = 0x0003;
pub const ETH_P_IP: u16       = 0x0800;
pub const ETH_P_ARP: u16      = 0x0806;
pub const ETH_P_IPV6: u16     = 0x86DD;
pub const ETH_P_8021Q: u16    = 0x8100;
pub const ETH_ALEN: usize     = 6;
pub const ETH_HLEN: usize     = 14;

// ============================================================================
// Packet socket state
// ============================================================================

pub struct PacketState {
    pub protocol: u16,           // ETH_P_* in network byte order
    pub ifindex: i32,            // bound interface index (0 = all)
    pub version: i32,            // TPACKET_V1/V2/V3
    pub auxdata: bool,           // PACKET_AUXDATA
    pub origdev: bool,           // PACKET_ORIGDEV
    pub vnet_hdr: bool,          // PACKET_VNET_HDR
    pub copy_thresh: u32,        // PACKET_COPY_THRESH
    pub reserve: u32,            // PACKET_RESERVE header space
    pub qdisc_bypass: bool,      // PACKET_QDISC_BYPASS
    pub ignore_outgoing: bool,   // PACKET_IGNORE_OUTGOING
    pub recv_queue: VecDeque<PacketFrame>,
    pub stats: PacketStats,
}

pub struct PacketFrame {
    pub data: Vec<u8>,
    pub sll: SockaddrLl,
    pub timestamp: u64,
}

/// struct sockaddr_ll — per Linux include/uapi/linux/if_packet.h
#[derive(Clone)]
pub struct SockaddrLl {
    pub sll_family: u16,     // AF_PACKET (17)
    pub sll_protocol: u16,   // ETH_P_* in network byte order
    pub sll_ifindex: i32,    // interface index
    pub sll_hatype: u16,     // ARP hardware type
    pub sll_pkttype: u8,     // packet type
    pub sll_halen: u8,       // address length
    pub sll_addr: [u8; 8],   // physical layer address
}

impl SockaddrLl {
    pub fn new() -> Self {
        Self {
            sll_family: 17,
            sll_protocol: 0,
            sll_ifindex: 0,
            sll_hatype: 0,
            sll_pkttype: 0,
            sll_halen: 0,
            sll_addr: [0; 8],
        }
    }

    /// Serialize to bytes (struct sockaddr_ll = 20 bytes)
    pub fn to_bytes(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0..2].copy_from_slice(&self.sll_family.to_ne_bytes());
        buf[2..4].copy_from_slice(&self.sll_protocol.to_be_bytes());
        buf[4..8].copy_from_slice(&self.sll_ifindex.to_ne_bytes());
        buf[8..10].copy_from_slice(&self.sll_hatype.to_ne_bytes());
        buf[10] = self.sll_pkttype;
        buf[11] = self.sll_halen;
        buf[12..20].copy_from_slice(&self.sll_addr);
        buf
    }
}

pub struct PacketStats {
    pub tp_packets: u32,
    pub tp_drops: u32,
    pub tp_freeze_q_cnt: u32,
}

impl PacketState {
    pub fn new(protocol: u16) -> Self {
        Self {
            protocol,
            ifindex: 0,
            version: TPACKET_V1,
            auxdata: false,
            origdev: false,
            vnet_hdr: false,
            copy_thresh: 0,
            reserve: 0,
            qdisc_bypass: false,
            ignore_outgoing: false,
            recv_queue: VecDeque::new(),
            stats: PacketStats {
                tp_packets: 0,
                tp_drops: 0,
                tp_freeze_q_cnt: 0,
            },
        }
    }
}

// ============================================================================
// Packet socket option handling — per Linux net/packet/af_packet.c
// ============================================================================

/// Handle setsockopt(SOL_PACKET, ...) — per Linux net/packet/af_packet.c packet_setsockopt
pub fn packet_setsockopt(socket: &Arc<SocketFile>, optname: i32, value: &[u8]) -> Result<(), SocketError> {
    let read_i32 = |v: &[u8]| -> Result<i32, SocketError> {
        if v.len() < 4 { return Err(SocketError::InvalidArg); }
        Ok(i32::from_ne_bytes(v[..4].try_into().unwrap()))
    };

    match optname {
        PACKET_ADD_MEMBERSHIP | PACKET_DROP_MEMBERSHIP => {
            // struct packet_mreq: ifindex(4) + type(2) + alen(2) + address(8) = 16 bytes
            if value.len() < 16 {
                return Err(SocketError::InvalidArg);
            }
            // Accept silently
            Ok(())
        }
        PACKET_VERSION => {
            let ver = read_i32(value)?;
            if ver < TPACKET_V1 || ver > TPACKET_V3 {
                return Err(SocketError::InvalidArg);
            }
            socket.with_state_mut(|state| state.packet_version = ver);
            Ok(())
        }
        PACKET_AUXDATA => {
            let val = read_i32(value)? != 0;
            socket.with_state_mut(|state| state.packet_auxdata = val);
            Ok(())
        }
        PACKET_ORIGDEV => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_VNET_HDR => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_COPY_THRESH => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_RESERVE => {
            let val = read_i32(value)?;
            if val < 0 {
                return Err(SocketError::InvalidArg);
            }
            Ok(())
        }
        PACKET_LOSS => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_TIMESTAMP => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_FANOUT => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_TX_HAS_OFF | PACKET_QDISC_BYPASS | PACKET_IGNORE_OUTGOING | PACKET_TX_TIMESTAMP => {
            let _val = read_i32(value)?;
            Ok(())
        }
        PACKET_RX_RING | PACKET_TX_RING => {
            // struct tpacket_req: block_size(4) + block_nr(4) + frame_size(4) + frame_nr(4)
            if value.len() < 16 {
                return Err(SocketError::InvalidArg);
            }
            // CVE-2017-7308: a TPACKET_V3 request (struct tpacket_req3) carries
            // tp_sizeof_priv at offset 20. The block must be large enough to
            // hold the per-block private area plus the block descriptor; a
            // huge tp_sizeof_priv (high bit set) would overflow BLK_PLUS_PRIV
            // and be wrongly accepted. Reject it with EINVAL (setsockopt02).
            if value.len() >= 24 {
                let tp_block_size =
                    u32::from_ne_bytes(value[0..4].try_into().unwrap()) as u64;
                let tp_sizeof_priv =
                    u32::from_ne_bytes(value[20..24].try_into().unwrap()) as u64;
                // sizeof(struct tpacket_block_desc) == 48 on Linux.
                const TPACKET_BLOCK_DESC: u64 = 48;
                if tp_sizeof_priv + TPACKET_BLOCK_DESC >= tp_block_size {
                    return Err(SocketError::InvalidArg);
                }
            }
            // Accept silently — we don't implement ring buffers
            Ok(())
        }
        _ => Err(SocketError::NoProtocolOption),
    }
}

/// Handle getsockopt(SOL_PACKET, ...) — per Linux net/packet/af_packet.c packet_getsockopt
pub fn packet_getsockopt(socket: &Arc<SocketFile>, optname: i32) -> Result<Vec<u8>, SocketError> {
    let i32_bytes = |v: i32| -> Vec<u8> { v.to_ne_bytes().to_vec() };

    match optname {
        PACKET_VERSION => {
            let ver = socket.with_state(|state| state.packet_version);
            Ok(i32_bytes(ver))
        }
        PACKET_HDRLEN => {
            let ver = socket.with_state(|state| state.packet_version);
            let hdrlen = match ver {
                TPACKET_V1 => 32,  // sizeof(struct tpacket_hdr)
                TPACKET_V2 => 32,  // sizeof(struct tpacket2_hdr)
                TPACKET_V3 => 48,  // sizeof(struct tpacket3_hdr)
                _ => 32,
            };
            Ok(i32_bytes(hdrlen))
        }
        PACKET_STATISTICS => {
            // struct tpacket_stats: tp_packets(4) + tp_drops(4)
            let mut stats = alloc::vec![0u8; 8];
            let (pkts, drops) = socket.with_state(|state| {
                (state.packet_stats_packets, state.packet_stats_drops)
            });
            stats[0..4].copy_from_slice(&pkts.to_ne_bytes());
            stats[4..8].copy_from_slice(&drops.to_ne_bytes());
            // Per Linux: reading stats resets them
            socket.with_state_mut(|state| {
                state.packet_stats_packets = 0;
                state.packet_stats_drops = 0;
            });
            Ok(stats)
        }
        PACKET_AUXDATA => {
            let val = socket.with_state(|state| state.packet_auxdata as i32);
            Ok(i32_bytes(val))
        }
        PACKET_ORIGDEV | PACKET_VNET_HDR | PACKET_COPY_THRESH |
        PACKET_RESERVE | PACKET_LOSS | PACKET_TIMESTAMP | PACKET_FANOUT |
        PACKET_TX_HAS_OFF | PACKET_QDISC_BYPASS | PACKET_IGNORE_OUTGOING => {
            Ok(i32_bytes(0))
        }
        _ => Err(SocketError::NoProtocolOption),
    }
}

// ============================================================================
// Packet socket bind — per Linux net/packet/af_packet.c packet_bind
// ============================================================================

/// Bind a packet socket to a specific interface and protocol.
/// sockaddr_ll: family(2) + protocol(2) + ifindex(4) + hatype(2) + pkttype(1) + halen(1) + addr(8) = 20 bytes
pub fn packet_bind(socket: &Arc<SocketFile>, addr: &[u8; 16], addrlen: usize) -> Result<(), SocketError> {
    if addrlen < 8 {
        return Err(SocketError::InvalidArg);
    }
    let _family = u16::from_ne_bytes([addr[0], addr[1]]);
    let protocol = u16::from_be_bytes([addr[2], addr[3]]);
    let ifindex = i32::from_ne_bytes([addr[4], addr[5], addr[6], addr[7]]);

    socket.with_state_mut(|state| {
        state.packet_protocol = protocol;
        state.packet_ifindex = ifindex;
        state.bound = true;
        state.bound_addr = *addr;
    });

    Ok(())
}

// ============================================================================
// Packet socket send/recv — per Linux net/packet/af_packet.c
// ============================================================================

/// Send a raw frame via a packet socket.
/// Per Linux: packet_sendmsg() — the user provides the complete L2 frame.
pub fn packet_send(
    socket: &Arc<SocketFile>,
    data: &[u8],
    dest: Option<[u8; 16]>,
    _flags: i32,
) -> Result<usize, SocketError> {
    // For loopback: if the destination ifindex is 1 (lo), loop back to self
    let ifindex = dest.map(|addr| {
        if addr.len() >= 8 {
            i32::from_ne_bytes([addr[4], addr[5], addr[6], addr[7]])
        } else {
            0
        }
    }).unwrap_or(socket.with_state(|state| state.packet_ifindex));

    if ifindex == 1 {
        // Loopback: deliver to own recv queue
        socket.with_state_mut(|state| {
            state.backend.loopback_mut().recv_push_back(data.to_vec(), [0u8; 16]);
        });
        socket.wake_readers();
    }

    Ok(data.len())
}

/// Receive a raw frame from a packet socket.
pub fn packet_recv(
    socket: &Arc<SocketFile>,
    buf: &mut [u8],
    flags: i32,
) -> Result<(usize, [u8; 16]), SocketError> {
    const MSG_PEEK: i32 = 2;
    const MSG_TRUNC: i32 = 0x20;
    const MSG_ERRQUEUE: i32 = 0x2000;

    if flags & MSG_ERRQUEUE != 0 {
        return Err(SocketError::WouldBlock);
    }

    let peek = flags & MSG_PEEK != 0;
    let trunc = flags & MSG_TRUNC != 0;

    socket.with_state_mut(|state| {
        let entry = if peek {
            state.backend.loopback().recv_buf.front().cloned()
        } else {
            state.backend.loopback_mut().recv_pop_front()
        };

        if let Some((data, src_addr)) = entry {
            let copy_len = buf.len().min(data.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            let ret_len = if trunc { data.len() } else { copy_len };
            Ok((ret_len, src_addr))
        } else {
            Err(SocketError::WouldBlock)
        }
    })
}
