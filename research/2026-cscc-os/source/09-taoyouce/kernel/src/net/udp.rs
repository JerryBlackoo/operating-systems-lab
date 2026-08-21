//! UDP protocol implementation — per Linux net/ipv4/udp.c
//!
//! Implements UDP datagram socket handling, multicast, and /proc/net/udp
//! formatting, matching Linux kernel behavior.
//!
//! Reference: https://github.com/torvalds/linux/blob/master/net/ipv4/udp.c
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/udp.h

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::socket::{SocketError, SocketFile};

// ============================================================================
// UDP socket options — per include/uapi/linux/udp.h
// ============================================================================

pub const UDP_CORK: i32           = 1;
pub const UDP_ENCAP: i32          = 100;
pub const UDP_NO_CHECK6_TX: i32   = 101;
pub const UDP_NO_CHECK6_RX: i32   = 102;
pub const UDP_SEGMENT: i32        = 103;
pub const UDP_GRO: i32            = 104;

// UDP encapsulation types
pub const UDP_ENCAP_ESPINUDP_NON_IKE: i32 = 1;
pub const UDP_ENCAP_ESPINUDP: i32          = 2;
pub const UDP_ENCAP_L2TPINUDP: i32         = 3;
pub const UDP_ENCAP_GTP0: i32              = 4;
pub const UDP_ENCAP_GTP1U: i32             = 5;

// ============================================================================
// UDP control block — per struct udp_sock in Linux
// ============================================================================

/// UDP socket state — mirrors relevant fields from Linux struct udp_sock
pub struct UdpControlBlock {
    // Socket options
    pub cork: bool,             // UDP_CORK — cork sends
    pub no_check6_tx: bool,     // UDP_NO_CHECK6_TX — disable checksum on TX
    pub no_check6_rx: bool,     // UDP_NO_CHECK6_RX — accept no-checksum on RX
    pub encap_type: i32,        // UDP_ENCAP type
    pub gso_size: u16,          // UDP_SEGMENT GSO size

    // Receive queue — each entry is (data, source_addr)
    pub recv_queue: VecDeque<UdpDatagram>,
    pub recv_queue_size: usize, // current total bytes in recv queue

    // Limits
    pub recv_buf_size: usize,   // SO_RCVBUF equivalent (default 212992)
    pub send_buf_size: usize,   // SO_SNDBUF equivalent (default 212992)

    // Multicast state
    pub multicast_ttl: u8,      // IP_MULTICAST_TTL (default 1)
    pub multicast_loop: bool,   // IP_MULTICAST_LOOP (default true)
    pub multicast_if: u32,      // IP_MULTICAST_IF (interface index, 0 = default)
    pub multicast_groups: Vec<MulticastMembership>,

    // Counters
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub errors_in: u64,         // InErrors
    pub no_ports: u64,          // NoPorts (received for unbound port)
}

/// A received UDP datagram with source address
pub struct UdpDatagram {
    pub data: Vec<u8>,
    pub source_addr: [u8; 16],  // sockaddr_in or sockaddr_in6
    pub timestamp: u64,
}

/// Multicast group membership — per struct ip_mreq/ip_mreqn
pub struct MulticastMembership {
    pub group_addr: u32,        // multicast group IP (network byte order)
    pub interface_addr: u32,    // local interface IP (0 = default)
    pub interface_index: u32,   // interface index (ip_mreqn only)
}

impl UdpControlBlock {
    pub fn new() -> Self {
        Self {
            cork: false,
            no_check6_tx: false,
            no_check6_rx: false,
            encap_type: 0,
            gso_size: 0,
            recv_queue: VecDeque::new(),
            recv_queue_size: 0,
            recv_buf_size: 212992,
            send_buf_size: 212992,
            multicast_ttl: 1,
            multicast_loop: true,
            multicast_if: 0,
            multicast_groups: Vec::new(),
            datagrams_in: 0,
            datagrams_out: 0,
            bytes_in: 0,
            bytes_out: 0,
            errors_in: 0,
            no_ports: 0,
        }
    }
}

// ============================================================================
// UDP operations for loopback
// ============================================================================

/// UDP send — deliver datagram to matching bound socket.
/// For loopback, this is direct queue insertion.
///
/// Per Linux net/ipv4/udp.c udp_sendmsg():
/// - If connected and no dest specified, use connected peer address
/// - If dest specified, use it (even if connected — Linux allows this for UDP)
/// - MSG_MORE: cork until explicit push
pub fn udp_sendto(
    socket: &Arc<SocketFile>,
    data: &[u8],
    dest: Option<[u8; 16]>,
    flags: i32,
) -> Result<usize, SocketError> {
    udp_sendto_owned(socket, data.to_vec(), dest, flags)
}

/// Ownership-taking UDP send: the payload `Vec` moves straight into the
/// receiver's queue with no per-datagram copy. UDP never blocks here, so no
/// buffer hand-back is needed on error.
pub fn udp_sendto_owned(
    socket: &Arc<SocketFile>,
    data: Vec<u8>,
    dest: Option<[u8; 16]>,
    _flags: i32,
) -> Result<usize, SocketError> {
    let (domain, _sock_type, bound_addr) = socket.with_state(|state| {
        (state.domain, state.sock_type, state.bound_addr)
    });

    let dest_addr = match dest {
        Some(addr) => addr,
        None => {
            // Use connected peer address
            socket.with_state(|state| {
                state.backend.loopback().peer_addr.ok_or(SocketError::NotConnected)
            })?
        }
    };

    // For loopback: find the socket bound to the destination port
    let dest_port = super::socket::port_of(&dest_addr);
    if dest_port == 0 && domain == 2 {
        return Err(SocketError::InvalidArg);
    }

    // Extract destination IP (network byte order u32) from sockaddr_in
    let dest_ip_be32 = u32::from_ne_bytes([dest_addr[4], dest_addr[5], dest_addr[6], dest_addr[7]]);

    // Multicast delivery: if dest is multicast (224.0.0.0/4), fan-out to all group members
    if super::multicast::is_multicast_addr(dest_ip_be32) {
        let multicast_loop = socket.with_state(|state| state.ip_multicast_loop);
        let delivered = super::multicast::multicast_deliver(
            dest_ip_be32,
            dest_port,
            &data,
            bound_addr,
            socket.socket_id(),
            multicast_loop,
        );
        // Per Linux: sendto to multicast always succeeds even if no receivers
        let _ = delivered;
        return Ok(data.len());
    }

    // Unicast: find single receiver bound to dest_port.
    // Fast path: a previously found *connected* receiver (pass-1 match — the
    // highest-priority demux result, so it stays correct no matter what other
    // sockets exist) is cached per sending socket and revalidated with one
    // state read, replacing the full registry scan on every datagram.
    let sender_port = super::socket::port_of(&bound_addr);
    let cached = socket.with_state(|state| {
        state.backend.loopback().udp_dest_cache.as_ref().and_then(
            |(cport, csport, weak)| {
                if *cport == dest_port && *csport == sender_port {
                    weak.upgrade()
                } else {
                    None
                }
            },
        )
    });
    let cached = cached.filter(|r| {
        r.socket_id() != socket.socket_id()
            && r.with_state(|state| {
                state.domain == 2
                    && state.sock_type == 2
                    && state.bound
                    && !state.closed
                    && super::socket::port_of(&state.bound_addr) == dest_port
                    && state
                        .backend
                        .loopback()
                        .peer_addr
                        .map(|peer| super::socket::port_of(&peer) == sender_port)
                        .unwrap_or(false)
            })
    });
    let receiver = match cached {
        Some(r) => Some(r),
        None => {
            let (receiver, connected_match) =
                find_udp_receiver(dest_port, socket.socket_id(), &bound_addr);
            if connected_match {
                if let Some(ref r) = receiver {
                    let weak = Arc::downgrade(r);
                    socket.with_state_mut(|state| {
                        state.backend.loopback_mut().udp_dest_cache =
                            Some((dest_port, sender_port, weak));
                    });
                }
            }
            receiver
        }
    };
    if crate::cmdline::udpdbg_enabled() {
        crate::println!(
            "[udpdbg] send sid={} src_port={} dest_port={} len={} receiver={}",
            socket.socket_id(),
            super::socket::port_of(&bound_addr),
            dest_port,
            data.len(),
            receiver.as_ref().map(|s| s.socket_id()).unwrap_or(0)
        );
    }

    if let Some(recv_socket) = receiver {
        let data_len = data.len();

        // SO_RCVBUF 语义（对标 Linux __udp_enqueue_schedule_skb）：接收队列
        // 积压超过 rcvbuf 时不再无界入队——否则 PARALLEL_UDP 多流灌包快于消费
        // 方排空，recv_buf 无界增长直到内核堆 OOM panic（LA iperf 后整机
        // halt、后续组全 0 的根因）。与 Linux 直接丢包不同的是：回环上发送方
        // 与接收方共享 CPU，先挂起发送方等接收方排空（背压），既保住吞吐
        // （接收侧带宽 = 排空速率，不白丢），又把内存钉在 rcvbuf 上限内；
        // 接收方始终不消费时按 Linux 语义丢包，sendto 仍返回成功。
        let accepted = recv_socket.with_state_mut(|state| {
            let cap = (state.rcvbuf as usize).max(212_992);
            let lb = state.backend.loopback_mut();
            let queued: usize = lb.recv_bytes;
            if queued + data_len > cap {
                false
            } else {
                lb.recv_push_back(data, bound_addr);
                true
            }
        });
        if accepted {
            socket.with_state_mut(|state| {
                state.backend.loopback_mut().udp_bp_giveup = None;
            });
            recv_socket.wake_readers();
            return Ok(data_len);
        }

        // 队列满。suspend 后本 syscall 会被整体重放（sepc 回退到 ecall），
        // 所以"放弃时刻"要挂在发送方 socket 上跨重启保留：首次记 now+200ms，
        // 重放后仍满且超时 → 丢包（接收方确实不消费，如 LTP 只发不收）。
        let now = crate::arch::asm_helpers::rdtime();
        let giveup = socket.with_state_mut(|state| {
            let lb = state.backend.loopback_mut();
            *lb.udp_bp_giveup.get_or_insert(
                now.saturating_add(crate::arch::consts::CLOCK_FREQ as u64 / 5),
            )
        });
        if now >= giveup {
            socket.with_state_mut(|state| {
                state.backend.loopback_mut().udp_bp_giveup = None;
            });
            recv_socket.wake_readers();
            return Ok(data_len); // 按 Linux 语义丢弃，sendto 报告成功
        }
        recv_socket.wake_readers();
        // 非阻塞 socket 不挂起（Linux UDP sendto 本就不因对端 rcvbuf 满阻塞）。
        let nonblocking = socket.with_state(|state| state.nonblocking);
        if nonblocking {
            return Ok(data_len);
        }
        let pid = crate::sched::process::HART_CURRENT_PID
            .load(core::sync::atomic::Ordering::Relaxed);
        if pid != 0 {
            super::socket::register_write_waiter_sock_after_wouldblock(
                &recv_socket,
                pid,
                Some(giveup),
            );
            crate::sched::process::enter_wait_channel("udp_sndbuf");
            crate::sched::process::suspend_current_and_run_next();
        }
        Ok(data_len)
    } else {
        // Per Linux: sendto to unbound port succeeds for UDP (fire-and-forget)
        // The packet is silently dropped, but sendto returns success
        Ok(data.len())
    }
}

/// UDP recv — dequeue next datagram from recv queue.
/// Returns (bytes_read, source_addr).
///
/// Per Linux net/ipv4/udp.c udp_recvmsg():
/// - MSG_PEEK: read without consuming
/// - MSG_TRUNC: return full datagram size even if buffer too small
/// - MSG_DONTWAIT: non-blocking
pub fn udp_recvfrom(
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

    let result = socket.with_state_mut(|state| {
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
    });
    // 排空了一个报文：唤醒因 rcvbuf 满而背压挂起的发送方。
    if !peek && result.is_ok() {
        socket.wake_writers();
    }
    result
}

/// Find a UDP socket bound to the given port, implementing Linux-style
/// UDP demultiplexing (per net/ipv4/udp.c __udp4_lib_lookup):
///
/// 1. Connected sockets (peer_addr set) match only if the sender matches
///    their connected peer address+port — highest priority.
/// 2. Unconnected sockets (no peer_addr) accept from anyone — fallback.
///
/// This is critical for parallel UDP (iperf -P 5): each connected socket
/// receives only from its specific peer, while the listener socket picks
/// up new connections.
/// Returns (receiver, connected_match): `connected_match` is true when the
/// receiver was a pass-1 hit (connected socket whose peer is the sender) —
/// the only kind that is safe to cache per-sender.
fn find_udp_receiver(port: u16, sender_id: usize, sender_addr: &[u8; 16]) -> (Option<Arc<SocketFile>>, bool) {
    if port == 0 {
        return (None, false);
    }
    let sender_port = super::socket::port_of(sender_addr);

    let candidates: Vec<Arc<SocketFile>> = super::socket::all_socket_files()
        .into_iter()
        .filter(|s| {
            s.socket_id() != sender_id &&
            s.with_state(|state| {
                state.domain == 2 &&
                state.sock_type == 2 &&
                state.bound &&
                !state.closed &&
                super::socket::port_of(&state.bound_addr) == port
            })
        })
        .collect();

    // Pass 1: prefer connected socket whose peer matches the sender
    for s in &candidates {
        let is_match = s.with_state(|state| {
            if let Some(peer) = state.backend.loopback().peer_addr {
                super::socket::port_of(&peer) == sender_port
            } else {
                false
            }
        });
        if is_match {
            return (Some(s.clone()), true);
        }
    }

    // Pass 2: fall back to unconnected (listener) socket
    for s in &candidates {
        let unconnected = s.with_state(|state| {
            state.backend.loopback().peer_addr.is_none()
        });
        if unconnected {
            return (Some(s.clone()), false);
        }
    }

    (None, false)
}

// ============================================================================
// UDP option handling — setsockopt/getsockopt at IPPROTO_UDP level
// ============================================================================

/// Handle setsockopt(IPPROTO_UDP, ...) — per Linux net/ipv4/udp.c udp_lib_setsockopt
pub fn udp_setsockopt(socket: &Arc<SocketFile>, optname: i32, value: &[u8]) -> Result<(), SocketError> {
    let read_i32 = |v: &[u8]| -> Result<i32, SocketError> {
        if v.len() < 4 { return Err(SocketError::InvalidArg); }
        Ok(i32::from_ne_bytes(v[..4].try_into().unwrap()))
    };

    match optname {
        UDP_CORK => {
            let val = read_i32(value)? != 0;
            socket.with_state_mut(|state| state.udp_cork = val);
            Ok(())
        }
        UDP_ENCAP => {
            let _val = read_i32(value)?;
            // Accept silently — we don't actually encapsulate
            Ok(())
        }
        UDP_NO_CHECK6_TX => {
            let _val = read_i32(value)?;
            Ok(())
        }
        UDP_NO_CHECK6_RX => {
            let _val = read_i32(value)?;
            Ok(())
        }
        UDP_SEGMENT => {
            let val = read_i32(value)?;
            if val < 0 || val > 65535 {
                return Err(SocketError::InvalidArg);
            }
            Ok(())
        }
        UDP_GRO => {
            let _val = read_i32(value)?;
            Ok(())
        }
        _ => Err(SocketError::NoProtocolOption),
    }
}

/// Handle getsockopt(IPPROTO_UDP, ...) — per Linux net/ipv4/udp.c udp_lib_getsockopt
pub fn udp_getsockopt(socket: &Arc<SocketFile>, optname: i32) -> Result<Vec<u8>, SocketError> {
    let i32_bytes = |v: i32| -> Vec<u8> { v.to_ne_bytes().to_vec() };

    match optname {
        UDP_CORK => {
            let val = socket.with_state(|state| state.udp_cork as i32);
            Ok(i32_bytes(val))
        }
        UDP_ENCAP => Ok(i32_bytes(0)),
        UDP_NO_CHECK6_TX => Ok(i32_bytes(0)),
        UDP_NO_CHECK6_RX => Ok(i32_bytes(0)),
        UDP_SEGMENT => Ok(i32_bytes(0)),
        UDP_GRO => Ok(i32_bytes(0)),
        _ => Err(SocketError::NoProtocolOption),
    }
}

// ============================================================================
// /proc/net/udp format helpers
// ============================================================================

/// Format a UDP socket entry for /proc/net/udp output.
/// Per Linux net/ipv4/udp.c udp4_seq_show()
pub fn format_procnet_udp_entry(
    slot: u32,
    local_ip: u32,
    local_port: u16,
    remote_ip: u32,
    remote_port: u16,
    state: u8,
    tx_queue: u32,
    rx_queue: u32,
    uid: u32,
    inode: u64,
) -> alloc::string::String {
    use alloc::format;
    format!(
        "{:4}: {:08X}:{:04X} {:08X}:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:5} {:8} {} {} {} {} {}\n",
        slot,
        local_ip.swap_bytes(), local_port,
        remote_ip.swap_bytes(), remote_port,
        state,
        tx_queue, rx_queue,
        0u8,       // timer_active
        0u32,      // timer_expires
        0u32,      // retransmits
        uid,
        0u32,      // timeout
        inode,
        0, 0, 0, 0
    )
}
