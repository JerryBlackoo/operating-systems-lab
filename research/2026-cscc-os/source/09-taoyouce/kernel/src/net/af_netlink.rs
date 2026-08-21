//! AF_NETLINK protocol family implementation — per Linux net/netlink/af_netlink.c
//!
//! Netlink is a socket-based IPC mechanism for communication between kernel
//! and userspace (and between userspace processes). It is used by iproute2,
//! udev, audit, SELinux, and many other subsystems.
//!
//! Reference: https://github.com/torvalds/linux/blob/master/net/netlink/af_netlink.c
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/netlink.h
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/rtnetlink.h

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::socket::{SocketError, SocketFile};

// ============================================================================
// Netlink protocol families — per include/uapi/linux/netlink.h
// ============================================================================

pub const NETLINK_ROUTE: i32          = 0;   // Routing/device hook
pub const NETLINK_UNUSED: i32         = 1;
pub const NETLINK_USERSOCK: i32       = 2;   // User-mode socket protocols
pub const NETLINK_FIREWALL: i32       = 3;   // Unused (was ip_queue)
pub const NETLINK_SOCK_DIAG: i32      = 4;   // Socket monitoring
pub const NETLINK_NFLOG: i32          = 5;   // Netfilter ULOG
pub const NETLINK_XFRM: i32          = 6;   // IPsec
pub const NETLINK_SELINUX: i32        = 7;
pub const NETLINK_ISCSI: i32          = 8;
pub const NETLINK_AUDIT: i32          = 9;
pub const NETLINK_FIB_LOOKUP: i32     = 10;
pub const NETLINK_CONNECTOR: i32      = 11;
pub const NETLINK_NETFILTER: i32      = 12;
pub const NETLINK_IP6_FW: i32         = 13;
pub const NETLINK_DNRTMSG: i32        = 14;
pub const NETLINK_KOBJECT_UEVENT: i32 = 15;
pub const NETLINK_GENERIC: i32        = 16;
pub const NETLINK_CRYPTO: i32         = 21;

pub const NETLINK_MAX: i32 = 32;

// ============================================================================
// Netlink message header — per include/uapi/linux/netlink.h
// ============================================================================

pub const NLMSG_ALIGNTO: usize = 4;
pub const NLMSG_HDRLEN: usize = 16; // sizeof(struct nlmsghdr)

pub const NLM_F_REQUEST: u16   = 0x001;
pub const NLM_F_MULTI: u16     = 0x002;
pub const NLM_F_ACK: u16       = 0x004;
pub const NLM_F_ECHO: u16      = 0x008;
pub const NLM_F_DUMP_INTR: u16 = 0x010;
pub const NLM_F_DUMP_FILTERED: u16 = 0x020;

// GET request flags
pub const NLM_F_ROOT: u16      = 0x100;
pub const NLM_F_MATCH: u16     = 0x200;
pub const NLM_F_ATOMIC: u16    = 0x400;
pub const NLM_F_DUMP: u16      = NLM_F_ROOT | NLM_F_MATCH;

// NEW request flags
pub const NLM_F_REPLACE: u16   = 0x100;
pub const NLM_F_EXCL: u16      = 0x200;
pub const NLM_F_CREATE: u16    = 0x400;
pub const NLM_F_APPEND: u16    = 0x800;

// Standard netlink message types
pub const NLMSG_NOOP: u16      = 0x1;
pub const NLMSG_ERROR: u16     = 0x2;
pub const NLMSG_DONE: u16      = 0x3;
pub const NLMSG_OVERRUN: u16   = 0x4;
pub const NLMSG_MIN_TYPE: u16  = 0x10;

// ============================================================================
// RTnetlink message types — per include/uapi/linux/rtnetlink.h
// ============================================================================

pub const RTM_NEWLINK: u16    = 16;
pub const RTM_DELLINK: u16    = 17;
pub const RTM_GETLINK: u16    = 18;
pub const RTM_SETLINK: u16    = 19;
pub const RTM_NEWADDR: u16    = 20;
pub const RTM_DELADDR: u16    = 21;
pub const RTM_GETADDR: u16    = 22;
pub const RTM_NEWROUTE: u16   = 24;
pub const RTM_DELROUTE: u16   = 25;
pub const RTM_GETROUTE: u16   = 26;
pub const RTM_NEWNEIGH: u16   = 28;
pub const RTM_DELNEIGH: u16   = 29;
pub const RTM_GETNEIGH: u16   = 30;
pub const RTM_NEWRULE: u16    = 32;
pub const RTM_DELRULE: u16    = 33;
pub const RTM_GETRULE: u16    = 34;

// Interface flags (Linux ARPHRD_*)
pub const ARPHRD_LOOPBACK: u16 = 772;
pub const ARPHRD_ETHER: u16    = 1;

// IFLA attributes
pub const IFLA_UNSPEC: u16     = 0;
pub const IFLA_ADDRESS: u16    = 1;
pub const IFLA_BROADCAST: u16  = 2;
pub const IFLA_IFNAME: u16     = 3;
pub const IFLA_MTU: u16        = 4;
pub const IFLA_LINK: u16       = 5;
pub const IFLA_QDISC: u16      = 6;
pub const IFLA_STATS: u16      = 7;
pub const IFLA_TXQLEN: u16     = 13;
pub const IFLA_OPERSTATE: u16  = 16;
pub const IFLA_LINKMODE: u16   = 17;
pub const IFLA_GROUP: u16      = 27;

// IFA attributes (address)
pub const IFA_UNSPEC: u16      = 0;
pub const IFA_ADDRESS: u16     = 1;
pub const IFA_LOCAL: u16       = 2;
pub const IFA_LABEL: u16       = 3;
pub const IFA_BROADCAST: u16   = 4;

// ============================================================================
// SOL_NETLINK options — per include/uapi/linux/netlink.h
// ============================================================================

pub const SOL_NETLINK: i32              = 270;
pub const NETLINK_ADD_MEMBERSHIP: i32   = 1;
pub const NETLINK_DROP_MEMBERSHIP: i32  = 2;
pub const NETLINK_PKTINFO: i32          = 3;
pub const NETLINK_BROADCAST_ERROR: i32  = 4;
pub const NETLINK_NO_ENOBUFS: i32       = 5;
pub const NETLINK_RX_RING: i32          = 6;
pub const NETLINK_TX_RING: i32          = 7;
pub const NETLINK_LISTEN_ALL_NSID: i32  = 8;
pub const NETLINK_LIST_MEMBERSHIPS: i32 = 9;
pub const NETLINK_CAP_ACK: i32          = 10;
pub const NETLINK_EXT_ACK: i32          = 11;
pub const NETLINK_GET_STRICT_CHK: i32   = 12;

// ============================================================================
// Netlink socket state
// ============================================================================

pub struct NetlinkState {
    pub protocol: i32,            // NETLINK_ROUTE, NETLINK_GENERIC, etc.
    pub portid: u32,              // Netlink port ID (usually PID)
    pub groups: u32,              // multicast group subscriptions (bitmask)
    pub dst_portid: u32,          // destination portid (0 = kernel)
    pub dst_group: u32,           // destination multicast group
    pub recv_queue: VecDeque<Vec<u8>>,  // received messages
    pub cap_ack: bool,            // NETLINK_CAP_ACK
    pub ext_ack: bool,            // NETLINK_EXT_ACK
    pub no_enobufs: bool,         // NETLINK_NO_ENOBUFS
    pub broadcast_error: bool,    // NETLINK_BROADCAST_ERROR
    pub strict_chk: bool,         // NETLINK_GET_STRICT_CHK
}

impl NetlinkState {
    pub fn new(protocol: i32, pid: u32) -> Self {
        Self {
            protocol,
            portid: pid,
            groups: 0,
            dst_portid: 0,
            dst_group: 0,
            recv_queue: VecDeque::new(),
            cap_ack: false,
            ext_ack: false,
            no_enobufs: false,
            broadcast_error: false,
            strict_chk: false,
        }
    }
}

// ============================================================================
// Netlink message building helpers
// ============================================================================

fn nlmsg_align(len: usize) -> usize {
    (len + NLMSG_ALIGNTO - 1) & !(NLMSG_ALIGNTO - 1)
}

fn nlattr_size(payload_len: usize) -> usize {
    nlmsg_align(4 + payload_len) // NLA header (4 bytes) + payload aligned
}

fn put_nlattr(buf: &mut Vec<u8>, nla_type: u16, data: &[u8]) {
    let nla_len = (4 + data.len()) as u16;
    buf.extend_from_slice(&nla_len.to_ne_bytes());
    buf.extend_from_slice(&nla_type.to_ne_bytes());
    buf.extend_from_slice(data);
    // Pad to alignment
    let aligned = nlmsg_align(4 + data.len());
    let pad = aligned - (4 + data.len());
    for _ in 0..pad {
        buf.push(0);
    }
}

fn put_nlmsghdr(buf: &mut Vec<u8>, msg_type: u16, flags: u16, seq: u32, pid: u32, payload_len: u32) {
    let nlmsg_len = (NLMSG_HDRLEN as u32) + payload_len;
    buf.extend_from_slice(&nlmsg_len.to_ne_bytes());
    buf.extend_from_slice(&msg_type.to_ne_bytes());
    buf.extend_from_slice(&flags.to_ne_bytes());
    buf.extend_from_slice(&seq.to_ne_bytes());
    buf.extend_from_slice(&pid.to_ne_bytes());
}

fn put_nlmsg_error(buf: &mut Vec<u8>, error: i32, seq: u32, pid: u32) {
    // nlmsgerr = { int error; struct nlmsghdr msg; }
    // For cap_ack: just the error code (4 bytes)
    // For full: error (4) + original nlmsghdr (16) = 20 bytes
    let payload_len = 4u32; // just error code (cap_ack style)
    put_nlmsghdr(buf, NLMSG_ERROR, 0, seq, pid, payload_len);
    buf.extend_from_slice(&error.to_ne_bytes());
}

fn put_nlmsg_done(buf: &mut Vec<u8>, seq: u32, pid: u32) {
    put_nlmsghdr(buf, NLMSG_DONE, NLM_F_MULTI, seq, pid, 4);
    buf.extend_from_slice(&0i32.to_ne_bytes()); // errno = 0
}

// ============================================================================
// RTnetlink: interface dump — per Linux net/core/rtnetlink.c rtnl_dump_ifinfo
// ============================================================================

/// Virtual network interfaces for RTnetlink responses
struct VirtualIface {
    index: u32,
    name: &'static str,
    mtu: u32,
    flags: u32,     // IFF_UP | IFF_RUNNING | IFF_LOOPBACK etc.
    arp_type: u16,  // ARPHRD_*
    hwaddr: [u8; 6],
    ipv4_addr: [u8; 4],
    ipv4_mask: u8,   // prefix length
    ipv6_addr: [u8; 16],
    ipv6_mask: u8,
}

const IFF_UP: u32        = 0x1;
const IFF_BROADCAST: u32 = 0x2;
const IFF_LOOPBACK: u32  = 0x8;
const IFF_RUNNING: u32   = 0x40;
const IFF_NOARP: u32     = 0x80;
const IFF_MULTICAST: u32 = 0x1000;
const IFF_LOWER_UP: u32  = 0x10000;

fn virtual_interfaces() -> [VirtualIface; 2] {
    [
        VirtualIface {
            index: 1,
            name: "lo",
            mtu: 65536,
            flags: IFF_UP | IFF_LOOPBACK | IFF_RUNNING | IFF_LOWER_UP,
            arp_type: ARPHRD_LOOPBACK,
            hwaddr: [0; 6],
            ipv4_addr: [127, 0, 0, 1],
            ipv4_mask: 8,
            ipv6_addr: [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1],
            ipv6_mask: 128,
        },
        VirtualIface {
            index: 2,
            name: "eth0",
            mtu: 1500,
            flags: IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST | IFF_LOWER_UP,
            arp_type: ARPHRD_ETHER,
            hwaddr: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            ipv4_addr: [10, 0, 2, 15],
            ipv4_mask: 24,
            ipv6_addr: [0xfe,0x80,0,0,0,0,0,0,0x50,0x54,0x00,0xff,0xfe,0x12,0x34,0x56],
            ipv6_mask: 64,
        },
    ]
}

/// Build RTM_NEWLINK dump response for all interfaces.
/// Per Linux net/core/rtnetlink.c rtnl_fill_ifinfo()
fn build_getlink_dump(seq: u32, pid: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);

    for iface in virtual_interfaces().iter() {
        // struct ifinfomsg (16 bytes)
        let mut ifinfo = Vec::with_capacity(256);
        ifinfo.push(2u8); // ifi_family = AF_INET
        ifinfo.push(0u8); // __ifi_pad
        ifinfo.extend_from_slice(&iface.arp_type.to_ne_bytes()); // ifi_type
        ifinfo.extend_from_slice(&(iface.index as i32).to_ne_bytes()); // ifi_index
        ifinfo.extend_from_slice(&iface.flags.to_ne_bytes()); // ifi_flags
        ifinfo.extend_from_slice(&0xFFFFFFFFu32.to_ne_bytes()); // ifi_change (all bits)

        // Attributes
        let name_with_null = {
            let mut v: Vec<u8> = iface.name.as_bytes().to_vec();
            v.push(0);
            v
        };
        put_nlattr(&mut ifinfo, IFLA_IFNAME, &name_with_null);
        put_nlattr(&mut ifinfo, IFLA_MTU, &iface.mtu.to_ne_bytes());
        put_nlattr(&mut ifinfo, IFLA_TXQLEN, &1000u32.to_ne_bytes());
        put_nlattr(&mut ifinfo, IFLA_OPERSTATE, &[6u8]); // IF_OPER_UP
        put_nlattr(&mut ifinfo, IFLA_LINKMODE, &[0u8]); // IF_LINK_MODE_DEFAULT
        put_nlattr(&mut ifinfo, IFLA_GROUP, &0u32.to_ne_bytes()); // default group
        // IFLA_QDISC: per Linux, pfifo_fast for real interfaces, noqueue for lo
        let qdisc_name = if iface.name == "lo" { b"noqueue\0" as &[u8] } else { b"pfifo_fast\0" as &[u8] };
        put_nlattr(&mut ifinfo, IFLA_QDISC, qdisc_name);
        if iface.hwaddr != [0; 6] {
            put_nlattr(&mut ifinfo, IFLA_ADDRESS, &iface.hwaddr);
            put_nlattr(&mut ifinfo, IFLA_BROADCAST, &[0xFF; 6]);
        }
        // IFLA_STATS: struct rtnl_link_stats (96 bytes, 24 x u32) — zero-init
        let stats = [0u8; 96];
        put_nlattr(&mut ifinfo, IFLA_STATS, &stats);

        // Build the nlmsghdr
        let payload_len = ifinfo.len() as u32;
        put_nlmsghdr(&mut buf, RTM_NEWLINK, NLM_F_MULTI, seq, pid, payload_len);
        buf.extend_from_slice(&ifinfo);
    }

    put_nlmsg_done(&mut buf, seq, pid);
    buf
}

/// Build RTM_NEWADDR dump response for all interface addresses.
/// Per Linux net/ipv4/devinet.c inet_fill_ifaddr()
fn build_getaddr_dump(seq: u32, pid: u32, family: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);

    for iface in virtual_interfaces().iter() {
        if family == 0 || family == 2 {
            // IPv4 address
            // struct ifaddrmsg (8 bytes)
            let mut ifa = Vec::with_capacity(64);
            ifa.push(2u8); // ifa_family = AF_INET
            ifa.push(iface.ipv4_mask); // ifa_prefixlen
            ifa.push(0u8); // ifa_flags
            ifa.push(0u8); // ifa_scope (RT_SCOPE_UNIVERSE for eth0, RT_SCOPE_HOST for lo)
            ifa.extend_from_slice(&(iface.index as i32).to_ne_bytes()); // ifa_index

            put_nlattr(&mut ifa, IFA_ADDRESS, &iface.ipv4_addr);
            put_nlattr(&mut ifa, IFA_LOCAL, &iface.ipv4_addr);
            let label_with_null = {
                let mut v: Vec<u8> = iface.name.as_bytes().to_vec();
                v.push(0);
                v
            };
            put_nlattr(&mut ifa, IFA_LABEL, &label_with_null);

            if iface.flags & IFF_BROADCAST != 0 {
                let bcast = [iface.ipv4_addr[0] | !(0xFFu8.wrapping_shl(32 - iface.ipv4_mask as u32)),
                             iface.ipv4_addr[1], iface.ipv4_addr[2], 255u8];
                put_nlattr(&mut ifa, IFA_BROADCAST, &bcast);
            }

            let payload_len = ifa.len() as u32;
            put_nlmsghdr(&mut buf, RTM_NEWADDR, NLM_F_MULTI, seq, pid, payload_len);
            buf.extend_from_slice(&ifa);
        }

        if family == 0 || family == 10 {
            // IPv6 address
            let mut ifa = Vec::with_capacity(64);
            ifa.push(10u8); // ifa_family = AF_INET6
            ifa.push(iface.ipv6_mask); // ifa_prefixlen
            ifa.push(0u8); // ifa_flags
            ifa.push(0u8); // ifa_scope
            ifa.extend_from_slice(&(iface.index as i32).to_ne_bytes()); // ifa_index

            put_nlattr(&mut ifa, IFA_ADDRESS, &iface.ipv6_addr);

            let payload_len = ifa.len() as u32;
            put_nlmsghdr(&mut buf, RTM_NEWADDR, NLM_F_MULTI, seq, pid, payload_len);
            buf.extend_from_slice(&ifa);
        }
    }

    put_nlmsg_done(&mut buf, seq, pid);
    buf
}

/// Build RTM_NEWROUTE dump response.
/// Per Linux net/ipv4/fib_frontend.c inet_dump_fib()
fn build_getroute_dump(seq: u32, pid: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);

    // Default route via eth0
    {
        let mut rt = Vec::with_capacity(64);
        // struct rtmsg (12 bytes)
        rt.push(2u8);   // rtm_family = AF_INET
        rt.push(0u8);   // rtm_dst_len = 0 (default route)
        rt.push(0u8);   // rtm_src_len
        rt.push(0u8);   // rtm_tos
        rt.push(254u8); // rtm_table = RT_TABLE_MAIN
        rt.push(3u8);   // rtm_protocol = RTPROT_BOOT
        rt.push(0u8);   // rtm_scope = RT_SCOPE_UNIVERSE
        rt.push(1u8);   // rtm_type = RTN_UNICAST
        rt.extend_from_slice(&0u32.to_ne_bytes()); // rtm_flags

        // Attributes
        const RTA_GATEWAY: u16 = 5;
        const RTA_OIF: u16 = 4;
        put_nlattr(&mut rt, RTA_GATEWAY, &[10u8, 0, 2, 2]); // 10.0.2.2 gateway
        put_nlattr(&mut rt, RTA_OIF, &2u32.to_ne_bytes()); // eth0 index

        let payload_len = rt.len() as u32;
        put_nlmsghdr(&mut buf, RTM_NEWROUTE, NLM_F_MULTI, seq, pid, payload_len);
        buf.extend_from_slice(&rt);
    }

    // Loopback route
    {
        let mut rt = Vec::with_capacity(64);
        rt.push(2u8);   // rtm_family
        rt.push(8u8);   // rtm_dst_len = 8 (127.0.0.0/8)
        rt.push(0u8);
        rt.push(0u8);
        rt.push(255u8); // rtm_table = RT_TABLE_LOCAL
        rt.push(2u8);   // rtm_protocol = RTPROT_KERNEL
        rt.push(254u8); // rtm_scope = RT_SCOPE_HOST
        rt.push(2u8);   // rtm_type = RTN_LOCAL
        rt.extend_from_slice(&0u32.to_ne_bytes());

        const RTA_DST: u16 = 1;
        const RTA_OIF: u16 = 4;
        put_nlattr(&mut rt, RTA_DST, &[127u8, 0, 0, 0]);
        put_nlattr(&mut rt, RTA_OIF, &1u32.to_ne_bytes()); // lo index

        let payload_len = rt.len() as u32;
        put_nlmsghdr(&mut buf, RTM_NEWROUTE, NLM_F_MULTI, seq, pid, payload_len);
        buf.extend_from_slice(&rt);
    }

    put_nlmsg_done(&mut buf, seq, pid);
    buf
}

// ============================================================================
// Netlink message processing — per Linux net/netlink/af_netlink.c
// ============================================================================

/// Process a netlink message sent by userspace.
/// Parse the nlmsghdr and route to the appropriate handler.
///
/// Per Linux net/netlink/af_netlink.c netlink_unicast() → netlink_rcv_skb()
pub fn netlink_process_message(
    socket: &Arc<SocketFile>,
    data: &[u8],
    portid: u32,
    protocol: i32,
) -> Result<(), SocketError> {
    if data.len() < NLMSG_HDRLEN {
        return Err(SocketError::InvalidArg);
    }

    let mut offset = 0;
    while offset + NLMSG_HDRLEN <= data.len() {
        let nlmsg_len = u32::from_ne_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
        let nlmsg_type = u16::from_ne_bytes(data[offset+4..offset+6].try_into().unwrap());
        let nlmsg_flags = u16::from_ne_bytes(data[offset+6..offset+8].try_into().unwrap());
        let nlmsg_seq = u32::from_ne_bytes(data[offset+8..offset+12].try_into().unwrap());
        let _nlmsg_pid = u32::from_ne_bytes(data[offset+12..offset+16].try_into().unwrap());

        if nlmsg_len < NLMSG_HDRLEN || offset + nlmsg_len > data.len() {
            break;
        }

        let _payload = &data[offset + NLMSG_HDRLEN..offset + nlmsg_len];

        if protocol == NETLINK_ROUTE {
            let response = match nlmsg_type {
                RTM_GETLINK => {
                    if nlmsg_flags & NLM_F_DUMP != 0 {
                        build_getlink_dump(nlmsg_seq, portid)
                    } else {
                        // Single link query — return error for now
                        let mut resp = Vec::new();
                        put_nlmsg_error(&mut resp, 0, nlmsg_seq, portid);
                        resp
                    }
                }
                RTM_GETADDR => {
                    let family = if offset + NLMSG_HDRLEN < data.len() {
                        data[offset + NLMSG_HDRLEN]
                    } else {
                        0
                    };
                    if nlmsg_flags & NLM_F_DUMP != 0 {
                        build_getaddr_dump(nlmsg_seq, portid, family)
                    } else {
                        let mut resp = Vec::new();
                        put_nlmsg_error(&mut resp, 0, nlmsg_seq, portid);
                        resp
                    }
                }
                RTM_GETROUTE => {
                    if nlmsg_flags & NLM_F_DUMP != 0 {
                        build_getroute_dump(nlmsg_seq, portid)
                    } else {
                        let mut resp = Vec::new();
                        put_nlmsg_error(&mut resp, 0, nlmsg_seq, portid);
                        resp
                    }
                }
                RTM_GETNEIGH => {
                    // Neighbor dump — return empty (done)
                    let mut resp = Vec::new();
                    put_nlmsg_done(&mut resp, nlmsg_seq, portid);
                    resp
                }
                RTM_GETRULE => {
                    let mut resp = Vec::new();
                    put_nlmsg_done(&mut resp, nlmsg_seq, portid);
                    resp
                }
                _ => {
                    // ACK unknown messages
                    if nlmsg_flags & NLM_F_ACK != 0 {
                        let mut resp = Vec::new();
                        put_nlmsg_error(&mut resp, 0, nlmsg_seq, portid);
                        resp
                    } else {
                        Vec::new()
                    }
                }
            };

            if !response.is_empty() {
                socket.with_state_mut(|state| {
                    state.backend.loopback_mut().recv_push_back(response, [0u8; 16]);
                });
                socket.wake_readers();
            }
        } else if protocol == NETLINK_KOBJECT_UEVENT || protocol == NETLINK_GENERIC
            || protocol == NETLINK_AUDIT || protocol == NETLINK_SOCK_DIAG {
            // ACK and return empty response
            if nlmsg_flags & NLM_F_ACK != 0 {
                let mut resp = Vec::new();
                put_nlmsg_error(&mut resp, 0, nlmsg_seq, portid);
                socket.with_state_mut(|state| {
                    state.backend.loopback_mut().recv_push_back(resp, [0u8; 16]);
                });
                socket.wake_readers();
            }
        }

        offset += nlmsg_align(nlmsg_len);
    }

    Ok(())
}

// ============================================================================
// SOL_NETLINK option handling
// ============================================================================

/// Handle setsockopt(SOL_NETLINK, ...) — per Linux net/netlink/af_netlink.c netlink_setsockopt
pub fn netlink_setsockopt(socket: &Arc<SocketFile>, optname: i32, value: &[u8]) -> Result<(), SocketError> {
    let read_i32 = |v: &[u8]| -> Result<i32, SocketError> {
        if v.len() < 4 { return Err(SocketError::InvalidArg); }
        Ok(i32::from_ne_bytes(v[..4].try_into().unwrap()))
    };

    match optname {
        NETLINK_ADD_MEMBERSHIP => {
            let group = read_i32(value)?;
            if group < 0 || group > 31 {
                return Err(SocketError::InvalidArg);
            }
            socket.with_state_mut(|state| {
                state.netlink_groups |= 1u32 << group;
            });
            Ok(())
        }
        NETLINK_DROP_MEMBERSHIP => {
            let group = read_i32(value)?;
            if group < 0 || group > 31 {
                return Err(SocketError::InvalidArg);
            }
            socket.with_state_mut(|state| {
                state.netlink_groups &= !(1u32 << group);
            });
            Ok(())
        }
        NETLINK_PKTINFO | NETLINK_BROADCAST_ERROR | NETLINK_NO_ENOBUFS |
        NETLINK_LISTEN_ALL_NSID | NETLINK_CAP_ACK | NETLINK_EXT_ACK |
        NETLINK_GET_STRICT_CHK => {
            let _val = read_i32(value)?;
            Ok(())
        }
        _ => Err(SocketError::NoProtocolOption),
    }
}

/// Handle getsockopt(SOL_NETLINK, ...)
pub fn netlink_getsockopt(socket: &Arc<SocketFile>, optname: i32) -> Result<Vec<u8>, SocketError> {
    let i32_bytes = |v: i32| -> Vec<u8> { v.to_ne_bytes().to_vec() };

    match optname {
        NETLINK_PKTINFO | NETLINK_BROADCAST_ERROR | NETLINK_NO_ENOBUFS |
        NETLINK_LISTEN_ALL_NSID | NETLINK_CAP_ACK | NETLINK_EXT_ACK |
        NETLINK_GET_STRICT_CHK => Ok(i32_bytes(0)),
        NETLINK_LIST_MEMBERSHIPS => {
            let groups = socket.with_state(|state| state.netlink_groups);
            Ok(groups.to_ne_bytes().to_vec())
        }
        _ => Err(SocketError::NoProtocolOption),
    }
}

// ============================================================================
// Netlink bind — per Linux net/netlink/af_netlink.c netlink_bind
// ============================================================================

/// Bind a netlink socket. The sockaddr_nl.nl_pid is the port ID.
/// Per Linux: if nl_pid == 0, kernel assigns current PID.
pub fn netlink_bind(socket: &Arc<SocketFile>, addr: &[u8; 16], addrlen: usize) -> Result<(), SocketError> {
    if addrlen < 12 {
        return Err(SocketError::InvalidArg);
    }
    // sockaddr_nl: family(2) + pad(2) + pid(4) + groups(4)
    let _family = u16::from_ne_bytes([addr[0], addr[1]]);
    let nl_pid = u32::from_ne_bytes([addr[4], addr[5], addr[6], addr[7]]);
    let nl_groups = u32::from_ne_bytes([addr[8], addr[9], addr[10], addr[11]]);

    socket.with_state_mut(|state| {
        state.netlink_portid = if nl_pid == 0 { state.owner_pid as u32 } else { nl_pid };
        state.netlink_groups = nl_groups;
        state.bound = true;
        state.bound_addr[0] = 16; // AF_NETLINK
        state.bound_addr[1] = 0;
        state.bound_addr[4..8].copy_from_slice(&state.netlink_portid.to_ne_bytes());
        state.bound_addr[8..12].copy_from_slice(&nl_groups.to_ne_bytes());
    });

    Ok(())
}
