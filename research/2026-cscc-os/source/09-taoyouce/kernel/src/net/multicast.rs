//! UDP/IP multicast group management — per Linux net/ipv4/igmp.c
//!
//! Implements:
//! - IGMP join/leave via smoltcp interface
//! - Multicast delivery for loopback: fan-out to all sockets in the group
//! - IP_MULTICAST_LOOP semantics
//!
//! Reference: https://github.com/torvalds/linux/blob/master/net/ipv4/igmp.c
//!            https://github.com/torvalds/linux/blob/master/net/ipv4/udp.c (udp_v4_mcast_deliver)

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::socket::SocketFile;
use super::net_stack;

/// Join a multicast group on the smoltcp interface (IGMP join).
/// Called from IP_ADD_MEMBERSHIP setsockopt handler.
pub fn join_multicast_group(group_addr_be32: u32) {
    let bytes = group_addr_be32.to_ne_bytes();
    let ip = smoltcp::wire::Ipv4Address::new(bytes[0], bytes[1], bytes[2], bytes[3]);
    net_stack::with_iface(|iface, device| {
        let _ = iface.join_multicast_group(device, ip, smoltcp::time::Instant::ZERO);
    });
}

/// Leave a multicast group on the smoltcp interface (IGMP leave).
/// Called from IP_DROP_MEMBERSHIP setsockopt handler.
pub fn leave_multicast_group(group_addr_be32: u32) {
    let bytes = group_addr_be32.to_ne_bytes();
    let ip = smoltcp::wire::Ipv4Address::new(bytes[0], bytes[1], bytes[2], bytes[3]);
    net_stack::with_iface(|iface, device| {
        let _ = iface.leave_multicast_group(device, ip, smoltcp::time::Instant::ZERO);
    });
}

/// Check if an IPv4 address (in network byte order u32) is multicast (224.0.0.0/4).
pub fn is_multicast_addr(addr_be32: u32) -> bool {
    let first_byte = addr_be32.to_ne_bytes()[0];
    (224..=239).contains(&first_byte)
}

/// Deliver a UDP datagram to all sockets that have joined the given multicast group
/// and are bound to the destination port. This implements Linux's `udp_v4_mcast_deliver`.
///
/// Returns the number of sockets the datagram was delivered to.
pub fn multicast_deliver(
    group_addr_be32: u32,
    dest_port: u16,
    data: &[u8],
    source_addr: [u8; 16],
    sender_id: usize,
    multicast_loop: bool,
) -> usize {
    let receivers = find_multicast_receivers(group_addr_be32, dest_port, sender_id, multicast_loop);
    let count = receivers.len();

    for recv_socket in receivers {
        // SO_RCVBUF 上限（与 udp_sendto_owned 同口径）：队列超限即丢，防多播
        // 灌包把 recv_buf 撑到内核堆 OOM。
        let accepted = recv_socket.with_state_mut(|state| {
            let cap = (state.rcvbuf as usize).max(212_992);
            let lb = state.backend.loopback_mut();
            let queued: usize = lb.recv_bytes;
            if queued + data.len() > cap {
                false
            } else {
                lb.recv_push_back(data.to_vec(), source_addr);
                true
            }
        });
        if accepted {
            recv_socket.wake_readers();
        }
    }

    count
}

/// Find all UDP sockets that have joined the given multicast group AND are bound
/// to the given destination port.
///
/// Per Linux: a socket receives multicast datagrams if:
/// 1. It is a UDP socket (sock_type == 2, domain == 2)
/// 2. It is bound (or bound to INADDR_ANY)
/// 3. Its bound port matches the destination port
/// 4. It has joined the multicast group via IP_ADD_MEMBERSHIP
/// 5. If sender_id matches (same socket) — only deliver if IP_MULTICAST_LOOP is set
fn find_multicast_receivers(
    group_addr_be32: u32,
    dest_port: u16,
    sender_id: usize,
    multicast_loop: bool,
) -> Vec<Arc<SocketFile>> {
    if dest_port == 0 {
        return Vec::new();
    }

    super::socket::all_socket_files()
        .into_iter()
        .filter(|s| {
            let sid = s.socket_id();
            // Skip sender unless multicast_loop is enabled
            if sid == sender_id && !multicast_loop {
                return false;
            }
            s.with_state(|state| {
                state.domain == 2
                    && state.sock_type == 2 // SOCK_DGRAM
                    && state.bound
                    && !state.closed
                    && super::socket::port_of(&state.bound_addr) == dest_port
                    && state.multicast_memberships.iter().any(|&(g, _)| g == group_addr_be32)
            })
        })
        .collect()
}
