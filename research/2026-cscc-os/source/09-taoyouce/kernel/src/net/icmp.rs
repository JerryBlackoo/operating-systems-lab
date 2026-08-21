//! ICMP protocol implementation — per Linux net/ipv4/icmp.c
//!
//! Implements ICMP echo request/reply (ping), destination unreachable, and
//! raw socket handling, matching Linux kernel behavior.
//!
//! Reference: https://github.com/torvalds/linux/blob/master/net/ipv4/icmp.c
//!            https://github.com/torvalds/linux/blob/master/net/ipv4/raw.c
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/icmp.h

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::socket::{SocketError, SocketFile};

// ============================================================================
// ICMP types — per include/uapi/linux/icmp.h
// ============================================================================

pub const ICMP_ECHOREPLY: u8        = 0;
pub const ICMP_DEST_UNREACH: u8     = 3;
pub const ICMP_SOURCE_QUENCH: u8    = 4;
pub const ICMP_REDIRECT: u8         = 5;
pub const ICMP_ECHO: u8             = 8;
pub const ICMP_TIME_EXCEEDED: u8    = 11;
pub const ICMP_PARAMETERPROB: u8    = 12;
pub const ICMP_TIMESTAMP: u8        = 13;
pub const ICMP_TIMESTAMPREPLY: u8   = 14;
pub const ICMP_INFO_REQUEST: u8     = 15;
pub const ICMP_INFO_REPLY: u8       = 16;
pub const ICMP_ADDRESS: u8          = 17;
pub const ICMP_ADDRESSREPLY: u8     = 18;

// ICMP Destination Unreachable codes — per RFC 792
pub const ICMP_NET_UNREACH: u8      = 0;
pub const ICMP_HOST_UNREACH: u8     = 1;
pub const ICMP_PROT_UNREACH: u8     = 2;
pub const ICMP_PORT_UNREACH: u8     = 3;
pub const ICMP_FRAG_NEEDED: u8      = 4;
pub const ICMP_SR_FAILED: u8        = 5;
pub const ICMP_NET_UNKNOWN: u8      = 6;
pub const ICMP_HOST_UNKNOWN: u8     = 7;
pub const ICMP_HOST_ISOLATED: u8    = 8;
pub const ICMP_NET_ANO: u8          = 9;
pub const ICMP_HOST_ANO: u8         = 10;
pub const ICMP_NET_UNR_TOS: u8      = 11;
pub const ICMP_HOST_UNR_TOS: u8     = 12;
pub const ICMP_PKT_FILTERED: u8     = 13;
pub const ICMP_PREC_VIOLATION: u8   = 14;
pub const ICMP_PREC_CUTOFF: u8      = 15;

// ============================================================================
// ICMPv6 types — per include/uapi/linux/icmpv6.h
// ============================================================================

pub const ICMPV6_DEST_UNREACH: u8          = 1;
pub const ICMPV6_PKT_TOOBIG: u8           = 2;
pub const ICMPV6_TIME_EXCEED: u8           = 3;
pub const ICMPV6_PARAMPROB: u8             = 4;
pub const ICMPV6_ECHO_REQUEST: u8          = 128;
pub const ICMPV6_ECHO_REPLY: u8            = 129;
pub const ICMPV6_MGM_QUERY: u8             = 130;
pub const ICMPV6_MGM_REPORT: u8            = 131;
pub const ICMPV6_MGM_REDUCTION: u8         = 132;
pub const ICMPV6_NI_QUERY: u8              = 139;
pub const ICMPV6_NI_REPLY: u8              = 140;

// ============================================================================
// ICMP header — per RFC 792
// ============================================================================

#[repr(C, packed)]
pub struct IcmpHeader {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub un: IcmpUn,
}

#[repr(C, packed)]
pub union IcmpUn {
    pub echo: IcmpEcho,
    pub gateway: u32,
    pub frag: IcmpFrag,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IcmpEcho {
    pub id: u16,
    pub sequence: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IcmpFrag {
    pub _unused: u16,
    pub mtu: u16,
}

// ============================================================================
// ICMP processing — per Linux net/ipv4/icmp.c
// ============================================================================

/// Process an incoming ICMP packet on the loopback interface.
/// For loopback, ICMP echo requests are answered with echo replies.
///
/// Per Linux net/ipv4/icmp.c icmp_rcv() → icmp_echo()
pub fn icmp_process_loopback(packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < 8 {
        return None;
    }

    let icmp_type = packet[0];
    let _code = packet[1];
    let _checksum = u16::from_be_bytes([packet[2], packet[3]]);

    match icmp_type {
        ICMP_ECHO => {
            // Build echo reply — swap type, recalculate checksum
            let mut reply = packet.to_vec();
            reply[0] = ICMP_ECHOREPLY;
            reply[1] = 0; // code = 0

            // Recalculate checksum
            reply[2] = 0;
            reply[3] = 0;
            let cksum = icmp_checksum(&reply);
            reply[2] = (cksum >> 8) as u8;
            reply[3] = (cksum & 0xff) as u8;

            Some(reply)
        }
        ICMPV6_ECHO_REQUEST => {
            // ICMPv6 echo reply
            let mut reply = packet.to_vec();
            reply[0] = ICMPV6_ECHO_REPLY;
            reply[2] = 0;
            reply[3] = 0;
            let cksum = icmp_checksum(&reply);
            reply[2] = (cksum >> 8) as u8;
            reply[3] = (cksum & 0xff) as u8;
            Some(reply)
        }
        _ => {
            // Other ICMP types: drop (no processing on loopback)
            None
        }
    }
}

/// Compute ICMP checksum — per RFC 1071
fn icmp_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

// ============================================================================
// Raw socket ICMP — per Linux net/ipv4/raw.c
// ============================================================================

/// Send an ICMP message via a raw socket (SOCK_RAW, IPPROTO_ICMP).
/// For loopback: if the destination is 127.0.0.1 / ::1, process locally.
///
/// Per Linux net/ipv4/raw.c raw_sendmsg():
/// - The caller provides the ICMP header + payload
/// - Kernel computes checksum if not provided
/// - For loopback, deliver to local raw socket
pub fn raw_icmp_send(
    socket: &Arc<SocketFile>,
    data: &[u8],
    dest_addr: [u8; 16],
    _flags: i32,
) -> Result<usize, SocketError> {
    if data.len() < 8 {
        return Err(SocketError::InvalidArg);
    }

    let len = data.len();

    // Check if destination is loopback
    let is_loopback = is_loopback_addr(&dest_addr);

    if is_loopback {
        // Process locally: ICMP echo → generate reply
        if let Some(reply) = icmp_process_loopback(data) {
            // Deliver reply to the sender's recv buffer
            let src_addr = socket.with_state(|state| state.bound_addr);
            socket.with_state_mut(|state| {
                state.backend.loopback_mut().recv_push_back(reply, src_addr);
            });
            socket.wake_readers();
        }
    }
    // For non-loopback destinations, packet is "sent" (silently dropped)

    Ok(len)
}

/// Receive an ICMP message from a raw socket.
/// Per Linux net/ipv4/raw.c raw_recvmsg()
pub fn raw_icmp_recv(
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

// ============================================================================
// ICMP statistics — per /proc/net/snmp IcmpMsg section
// ============================================================================

/// ICMP statistics counters — per Linux include/net/snmp.h ICMP_MIB_*
pub struct IcmpStats {
    pub in_msgs: u64,
    pub in_errors: u64,
    pub in_dest_unreachs: u64,
    pub in_time_excds: u64,
    pub in_parm_probs: u64,
    pub in_src_quenchs: u64,
    pub in_redirects: u64,
    pub in_echos: u64,
    pub in_echo_reps: u64,
    pub in_timestamps: u64,
    pub in_timestamp_reps: u64,
    pub in_addr_masks: u64,
    pub in_addr_mask_reps: u64,
    pub out_msgs: u64,
    pub out_errors: u64,
    pub out_dest_unreachs: u64,
    pub out_time_excds: u64,
    pub out_parm_probs: u64,
    pub out_src_quenchs: u64,
    pub out_redirects: u64,
    pub out_echos: u64,
    pub out_echo_reps: u64,
    pub out_timestamps: u64,
    pub out_timestamp_reps: u64,
    pub out_addr_masks: u64,
    pub out_addr_mask_reps: u64,
}

impl IcmpStats {
    pub fn new() -> Self {
        Self {
            in_msgs: 0, in_errors: 0, in_dest_unreachs: 0, in_time_excds: 0,
            in_parm_probs: 0, in_src_quenchs: 0, in_redirects: 0,
            in_echos: 0, in_echo_reps: 0, in_timestamps: 0, in_timestamp_reps: 0,
            in_addr_masks: 0, in_addr_mask_reps: 0,
            out_msgs: 0, out_errors: 0, out_dest_unreachs: 0, out_time_excds: 0,
            out_parm_probs: 0, out_src_quenchs: 0, out_redirects: 0,
            out_echos: 0, out_echo_reps: 0, out_timestamps: 0, out_timestamp_reps: 0,
            out_addr_masks: 0, out_addr_mask_reps: 0,
        }
    }
}

// ============================================================================
// Helper: check if an address is loopback
// ============================================================================

/// Check if an address is loopback. addr is a 16-byte kernel-internal sockaddr
/// where AF_INET stores sin_addr at [4..8] and AF_INET6 stores the first 12 bytes
/// of in6_addr at [4..16] (the full 16-byte in6_addr doesn't fit in 16-byte storage).
pub fn is_loopback_addr(addr: &[u8; 16]) -> bool {
    let family = u16::from_ne_bytes([addr[0], addr[1]]);
    match family {
        2 => {
            // AF_INET: 127.x.x.x (sin_addr at offset 4)
            addr[4] == 127
        }
        10 => {
            // AF_INET6: ::1 — in our 16-byte storage, in6_addr starts at [4]
            // and we can only store bytes [4..16] = first 12 bytes of the 16-byte addr.
            // ::1 = all zeros except the last byte. With only 12 bytes stored,
            // we check [4..15] == 0 and [15] == 0 (the real ::1 has byte[15]=1
            // but that maps to addr[19] which we can't store). So we treat any
            // all-zeros in the truncated window as potential loopback.
            // Better: check if it looks like loopback from the stored portion.
            let all_zero = addr[4..16].iter().all(|&b| b == 0);
            // IPv4-mapped ::ffff:127.x.x.x: bytes [4..14]=0, [14]=0xff, [15]=0xff
            let is_v4_mapped_lo = addr[4..14].iter().all(|&b| b == 0)
                && addr[14] == 0xff && addr[15] == 0xff;
            all_zero || is_v4_mapped_lo
        }
        _ => false,
    }
}
