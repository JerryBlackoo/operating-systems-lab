//! /proc/net/ pseudo-filesystem entries
//!
//! Implements the Linux /proc/net/ directory entries that expose network
//! subsystem state to userspace. Mirrors the format defined in:
//!   - net/ipv4/tcp_ipv4.c  (tcp4_seq_show)
//!   - net/ipv4/udp.c       (udp4_seq_show)
//!   - net/unix/af_unix.c   (unix_seq_show)
//!   - net/ipv4/proc.c      (sockstat_seq_show, snmp_seq_show, netstat_seq_show)
//!   - net/core/net-procfs.c (dev_seq_show)
//!   - net/ipv4/route.c     (fib_trie_seq_show — /proc/net/route)
//!   - net/if_inet6.c       (/proc/net/if_inet6)
//!   - net/ipv4/protocol.c  (/proc/net/protocols)
//!
//! Each generator implements `ProcFileGenerator` so procfs can serve them.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

use crate::fs::procfs::{ProcFileGenerator, ProcInode};

/// /proc/sys/net/core/somaxconn —— listen(2) backlog hard limit.
pub struct SomaxconnGenerator;

impl ProcFileGenerator for SomaxconnGenerator {
    fn generate(&self) -> String { String::from("4096\n") }
}

pub fn build_proc_net_dir() -> ProcInode {
    // Mirrors net/ipv4/proc.c, net/core/net-procfs.c, net/unix/af_unix.c, etc.
    let mut net_dir = ProcInode::directory("net");
    net_dir.add_child(Arc::new(ProcInode::file("tcp", Box::new(ProcNetTcpGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("tcp6", Box::new(ProcNetTcp6Generator))));
    net_dir.add_child(Arc::new(ProcInode::file("udp", Box::new(ProcNetUdpGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("udp6", Box::new(ProcNetUdp6Generator))));
    net_dir.add_child(Arc::new(ProcInode::file("unix", Box::new(ProcNetUnixGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("sockstat", Box::new(ProcNetSockstatGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("sockstat6", Box::new(ProcNetSockstat6Generator))));
    net_dir.add_child(Arc::new(ProcInode::file("netstat", Box::new(ProcNetNetstatGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("snmp", Box::new(ProcNetSnmpGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("dev", Box::new(ProcNetDevGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("route", Box::new(ProcNetRouteGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("if_inet6", Box::new(ProcNetIfInet6Generator))));
    net_dir.add_child(Arc::new(ProcInode::file("protocols", Box::new(ProcNetProtocolsGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("arp", Box::new(ProcNetArpGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("raw", Box::new(ProcNetRawGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("raw6", Box::new(ProcNetRaw6Generator))));
    net_dir.add_child(Arc::new(ProcInode::file("packet", Box::new(ProcNetPacketGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("netlink", Box::new(ProcNetNetlinkGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("fib_trie", Box::new(ProcNetFibTrieGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("fib_triestat", Box::new(ProcNetFibTrieStatGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("ptype", Box::new(ProcNetPtypeGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("wireless", Box::new(ProcNetWirelessGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("igmp", Box::new(ProcNetIgmpGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("igmp6", Box::new(ProcNetIgmp6Generator))));
    net_dir.add_child(Arc::new(ProcInode::file("psched", Box::new(ProcNetPschedGenerator))));
    net_dir.add_child(Arc::new(ProcInode::file("nf_conntrack", Box::new(ProcNetNfConntrackGenerator))));

    let mut net_stat_dir = ProcInode::directory("stat");
    net_stat_dir.add_child(Arc::new(ProcInode::file(
        "nf_conntrack",
        Box::new(ProcNetStatNfConntrackGenerator),
    )));
    net_dir.add_child(Arc::new(net_stat_dir));
    net_dir
}

// ─── /proc/net/tcp ──────────────────────────────────────────────────────────
// Linux format (tcp4_seq_show in net/ipv4/tcp_ipv4.c):
//   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
//
// Each field is hex-encoded per Linux convention. State codes:
//   01=ESTABLISHED 02=SYN_SENT 03=SYN_RECV 04=FIN_WAIT1 05=FIN_WAIT2
//   06=TIME_WAIT 07=CLOSE 08=CLOSE_WAIT 09=LAST_ACK 0A=LISTEN 0B=CLOSING

pub struct ProcNetTcpGenerator;

impl ProcFileGenerator for ProcNetTcpGenerator {
    fn generate(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");

        let sockets = super::socket::all_socket_files();
        let mut sl: u32 = 0;
        for sock in &sockets {
            sock.with_state(|state| {
                // Only AF_INET / AF_INET6 + SOCK_STREAM
                if (state.domain != 2 && state.reported_domain != 2
                    && state.reported_domain != 10)
                    || state.sock_type != 1
                {
                    return;
                }
                let local_ip = ip_from_bound(&state.bound_addr);
                let local_port = super::socket::port_of(&state.bound_addr);
                // peer address: we don't track remote addr fully, use 0
                let rem_ip: u32 = 0;
                let rem_port: u16 = 0;

                let st: u8 = if state.listening {
                    0x0A // LISTEN
                } else if state.closed {
                    0x07 // CLOSE
                } else if state.peer_pid.is_some() {
                    0x01 // ESTABLISHED
                } else {
                    0x07 // CLOSE
                };

                // Linux /proc/net/tcp uses little-endian hex for IP addresses
                let local_ip_le = local_ip.swap_bytes();
                let rem_ip_le = rem_ip.swap_bytes();

                out.push_str(&format!(
                    "{:4}: {:08X}:{:04X} {:08X}:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:5} {:8} {}\n",
                    sl,
                    local_ip_le, local_port,
                    rem_ip_le, rem_port,
                    st,
                    0u32, 0u32,  // tx_queue, rx_queue
                    0u8, 0u32,   // timer, when
                    0u32,        // retransmit
                    state.owner_pid,
                    0u32,        // timeout
                    0u64,        // inode
                ));
                sl += 1;
            });
        }
        out
    }
}

// ─── /proc/net/tcp6 ─────────────────────────────────────────────────────────
pub struct ProcNetTcp6Generator;

impl ProcFileGenerator for ProcNetTcp6Generator {
    fn generate(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");
        // We don't have real IPv6 sockets; emit empty table with correct header
        let sockets = super::socket::all_socket_files();
        let mut sl: u32 = 0;
        for sock in &sockets {
            sock.with_state(|state| {
                if state.reported_domain != 10 || state.sock_type != 1 {
                    return;
                }
                let st: u8 = if state.listening { 0x0A } else if state.peer_pid.is_some() { 0x01 } else { 0x07 };
                // IPv4-mapped IPv6: ::ffff:a.b.c.d
                let local_ip = ip_from_bound(&state.bound_addr);
                let local_port = super::socket::port_of(&state.bound_addr);
                out.push_str(&format!(
                    "{:4}: 00000000000000000000FFFF{:08X}:{:04X} 00000000000000000000000000000000:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:5} {:8} {}\n",
                    sl,
                    local_ip.swap_bytes(), local_port,
                    0u16,
                    st,
                    0u32, 0u32, 0u8, 0u32, 0u32,
                    state.owner_pid, 0u32, 0u64,
                ));
                sl += 1;
            });
        }
        out
    }
}

// ─── /proc/net/udp ──────────────────────────────────────────────────────────
// Linux format (udp4_seq_show in net/ipv4/udp.c):
//   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops

pub struct ProcNetUdpGenerator;

impl ProcFileGenerator for ProcNetUdpGenerator {
    fn generate(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n");

        let sockets = super::socket::all_socket_files();
        let mut sl: u32 = 0;
        for sock in &sockets {
            sock.with_state(|state| {
                if (state.domain != 2 && state.reported_domain != 2
                    && state.reported_domain != 10)
                    || state.sock_type != 2  // SOCK_DGRAM
                {
                    return;
                }
                let local_ip = ip_from_bound(&state.bound_addr);
                let local_port = super::socket::port_of(&state.bound_addr);
                let local_ip_le = local_ip.swap_bytes();

                // UDP state: 07 = CLOSE (unconnected), 01 = ESTABLISHED (connected)
                let st: u8 = if state.peer_pid.is_some() { 0x01 } else { 0x07 };

                out.push_str(&format!(
                    "{:4}: {:08X}:{:04X} {:08X}:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:5} {:8} {} {} {:p} {}\n",
                    sl,
                    local_ip_le, local_port,
                    0u32, 0u16,
                    st,
                    0u32, 0u32,
                    0u8, 0u32,
                    0u32,
                    state.owner_pid,
                    0u32,
                    0u64,       // inode
                    2u32,       // ref
                    core::ptr::null::<u8>(),
                    0u32,       // drops
                ));
                sl += 1;
            });
        }
        out
    }
}

// ─── /proc/net/udp6 ─────────────────────────────────────────────────────────
pub struct ProcNetUdp6Generator;

impl ProcFileGenerator for ProcNetUdp6Generator {
    fn generate(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n");
        // Empty for now — we don't have real IPv6 UDP sockets
        out
    }
}

// ─── /proc/net/unix ─────────────────────────────────────────────────────────
// Linux format (unix_seq_show in net/unix/af_unix.c):
//   Num       RefCount Protocol Flags    Type St Inode Path

pub struct ProcNetUnixGenerator;

impl ProcFileGenerator for ProcNetUnixGenerator {
    fn generate(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Num       RefCount Protocol Flags    Type St Inode Path\n");

        let sockets = super::socket::all_socket_files();
        for sock in &sockets {
            sock.with_state(|state| {
                if state.domain != 1 { // AF_UNIX only
                    return;
                }
                // Type: 1=STREAM, 2=DGRAM, 5=SEQPACKET
                let type_val = state.sock_type as u32;
                // State: 01=UNCONNECTED, 02=CONNECTING, 03=CONNECTED, 04=DISCONNECTING
                let st: u32 = if state.peer_pid.is_some() { 3 } else if state.listening { 1 } else { 1 };
                // Flags: 0x00010000 = ACC (accepting connections)
                let flags: u32 = if state.listening { 0x00010000 } else { 0 };

                // Path extraction from bound_addr
                let path = if state.bound {
                    let end = state.bound_addr[2..]
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(14);
                    let bytes = &state.bound_addr[2..2 + end];
                    if bytes.is_empty() {
                        String::new()
                    } else if bytes[0] == 0 {
                        // Abstract socket: show @name
                        let mut s = String::from("@");
                        for &b in &bytes[1..] {
                            if b == 0 { break; }
                            s.push(b as char);
                        }
                        s
                    } else {
                        String::from_utf8_lossy(bytes).into_owned()
                    }
                } else {
                    String::new()
                };

                out.push_str(&format!(
                    "{:p}: {:08X} {:08X} {:08X} {:04X} {:02X} {:5} {}\n",
                    sock as *const _ as *const u8,
                    2u32,      // RefCount
                    0u32,      // Protocol
                    flags,
                    type_val,
                    st,
                    0u32,      // Inode
                    path,
                ));
            });
        }
        out
    }
}

// ─── /proc/net/sockstat ─────────────────────────────────────────────────────
// Linux format (sockstat_seq_show in net/ipv4/proc.c):
//   sockets: used N
//   TCP: inuse N orphan N tw N alloc N mem N
//   UDP: inuse N mem N
//   UDPLITE: inuse N
//   RAW: inuse N
//   FRAG: inuse N memory N

pub struct ProcNetSockstatGenerator;

impl ProcFileGenerator for ProcNetSockstatGenerator {
    fn generate(&self) -> String {
        let sockets = super::socket::all_socket_files();
        let total = sockets.len();
        let mut tcp_inuse = 0u32;
        let mut udp_inuse = 0u32;
        let mut raw_inuse = 0u32;
        for sock in &sockets {
            sock.with_state(|state| {
                if state.domain == 1 { return; } // skip AF_UNIX
                match state.sock_type {
                    1 => tcp_inuse += 1, // SOCK_STREAM
                    2 => udp_inuse += 1, // SOCK_DGRAM
                    3 => raw_inuse += 1, // SOCK_RAW
                    _ => {}
                }
            });
        }
        format!(
            "sockets: used {}\n\
             TCP: inuse {} orphan 0 tw 0 alloc {} mem 0\n\
             UDP: inuse {} mem 0\n\
             UDPLITE: inuse 0\n\
             RAW: inuse {}\n\
             FRAG: inuse 0 memory 0\n",
            total, tcp_inuse, tcp_inuse, udp_inuse, raw_inuse,
        )
    }
}

// ─── /proc/net/sockstat6 ────────────────────────────────────────────────────
pub struct ProcNetSockstat6Generator;

impl ProcFileGenerator for ProcNetSockstat6Generator {
    fn generate(&self) -> String {
        String::from(
            "TCP6: inuse 0\n\
             UDP6: inuse 0\n\
             UDPLITE6: inuse 0\n\
             RAW6: inuse 0\n\
             FRAG6: inuse 0 memory 0\n"
        )
    }
}

// ─── /proc/net/netstat ──────────────────────────────────────────────────────
// Linux format (netstat_seq_show in net/ipv4/proc.c):
// Two lines: header with names, values line with numbers.
// TcpExt: SyncookiesSent SyncookiesRecv ... (many counters)
// TcpExt: 0 0 ...

pub struct ProcNetNetstatGenerator;

impl ProcFileGenerator for ProcNetNetstatGenerator {
    fn generate(&self) -> String {
        // Minimal but structurally correct netstat
        String::from(
            "TcpExt: SyncookiesSent SyncookiesRecv SyncookiesFailed EmbryonicRsts PruneCalled RcvPruned OfoPruned OutOfWindowIcmps LockDroppedIcmps ArpFilter TW TWRecycled TWKilled PAWSActive PAWSEstab DelayedACKs DelayedACKLocked DelayedACKLost ListenOverflows ListenDrops TCPHPHits TCPPureAcks TCPHPAcks\n\
             TcpExt: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             IpExt: InNoRoutes InTruncatedPkts InMcastPkts OutMcastPkts InBcastPkts OutBcastPkts InOctets OutOctets InMcastOctets OutMcastOctets InBcastOctets OutBcastOctets InCsumErrors InNoECTPkts InECT1Pkts InECT0Pkts InCEPkts\n\
             IpExt: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
        )
    }
}

// ─── /proc/net/snmp ─────────────────────────────────────────────────────────
// Linux format (snmp_seq_show in net/ipv4/proc.c):
// Paired header+values lines for Ip, Icmp, IcmpMsg, Tcp, Udp, UdpLite

pub struct ProcNetSnmpGenerator;

impl ProcFileGenerator for ProcNetSnmpGenerator {
    fn generate(&self) -> String {
        String::from(
            "Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs ReasmFails FragOKs FragFails FragCreates\n\
             Ip: 1 64 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             Icmp: InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs InRedirects InEchos InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs OutErrors OutRateLimitGlobal OutRateLimitHost OutDestUnreachs OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos OutEchoReps OutTimestamps OutTimestampReps OutAddrMasks OutAddrMaskReps\n\
             Icmp: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             IcmpMsg:\n\
             IcmpMsg:\n\
             Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\n\
             Tcp: 1 200 120000 -1 0 0 0 0 0 0 0 0 0 0 0\n\
             Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors\n\
             Udp: 0 0 0 0 0 0 0 0 0\n\
             UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors\n\
             UdpLite: 0 0 0 0 0 0 0 0 0\n"
        )
    }
}

// ─── /proc/net/dev ──────────────────────────────────────────────────────────
// Linux format (dev_seq_show in net/core/net-procfs.c):
// Inter-|   Receive                                                |  Transmit
//  face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed

pub struct ProcNetDevGenerator;

impl ProcFileGenerator for ProcNetDevGenerator {
    fn generate(&self) -> String {
        format!(
            "Inter-|   Receive                                                |  Transmit\n \
             face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
             lo: {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>10} {:>9} {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>7} {:>10}\n  \
             eth0: {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>10} {:>9} {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>7} {:>10}\n",
            // lo: all zeros (loopback)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            // eth0: all zeros
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        )
    }
}

// ─── /proc/net/route ────────────────────────────────────────────────────────
// Linux format (fib_trie_show_route in net/ipv4/route.c):
//   Iface  Destination  Gateway  Flags  RefCnt  Use  Metric  Mask  MTU  Window  IRTT

pub struct ProcNetRouteGenerator;

impl ProcFileGenerator for ProcNetRouteGenerator {
    fn generate(&self) -> String {
        // Default route via 10.0.2.2 (QEMU user-mode networking default)
        // and loopback route
        String::from(
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
             eth0\t00000000\t0202000A\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
             eth0\t0000020A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
             lo\t0000007F\t00000000\t0001\t0\t0\t0\t000000FF\t0\t0\t0\n"
        )
    }
}

// ─── /proc/net/if_inet6 ─────────────────────────────────────────────────────
// Linux format (if6_seq_show in net/ipv6/addrconf.c):
//   addr                            ifindex prefix_len scope flags ifname
// Example:
//   00000000000000000000000000000001 01 80 10 80 lo
//   fe80000000000000...              02 40 20 80 eth0

pub struct ProcNetIfInet6Generator;

impl ProcFileGenerator for ProcNetIfInet6Generator {
    fn generate(&self) -> String {
        // Loopback ::1 and link-local fe80::1 on eth0
        String::from(
            "00000000000000000000000000000001 01 80 10 80       lo\n\
             fe800000000000000000000000000001 02 40 20 80     eth0\n"
        )
    }
}

// ─── /proc/net/protocols ────────────────────────────────────────────────────
// Linux format (proto_seq_show in net/core/sock.c):
// protocol  size  sockets memory press maxhdr  slab module     ...

pub struct ProcNetProtocolsGenerator;

impl ProcFileGenerator for ProcNetProtocolsGenerator {
    fn generate(&self) -> String {
        String::from(
            "protocol  size sockets  memory press maxhdr  slab module     cl co di ac io in de sh ss gs se re sp bi br ha uh gp em\n\
             UNIX       1024      -1       -1   NI       0   no   kernel   y  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n\n\
             TCP        2048      -1       -1   no     320  yes   kernel   y  y  y  y  y  y  y  y  y  y  y  y  y  n  y  y  y  y  y\n\
             UDP        1024      -1       -1   NI     280  yes   kernel   y  y  y  n  y  y  y  n  y  y  y  y  n  n  y  y  y  y  n\n\
             RAW        1024      -1       -1   NI     280  yes   kernel   y  y  y  n  y  y  y  n  y  y  y  y  n  n  y  y  y  n  n\n\
             PING        896      -1       -1   NI       0  yes   kernel   y  y  y  n  n  y  n  n  y  y  y  y  n  n  n  y  y  y  n\n"
        )
    }
}

// ─── /proc/net/arp ──────────────────────────────────────────────────────────
// Linux format (arp_seq_show in net/ipv4/arp.c):
// IP address       HW type     Flags       HW address            Mask     Device

pub struct ProcNetArpGenerator;

impl ProcFileGenerator for ProcNetArpGenerator {
    fn generate(&self) -> String {
        // Default gateway entry for QEMU user-mode networking
        String::from(
            "IP address       HW type     Flags       HW address            Mask     Device\n\
             10.0.2.2         0x1         0x2         52:54:00:12:35:02     *        eth0\n"
        )
    }
}

// ─── /proc/net/raw ──────────────────────────────────────────────────────────
pub struct ProcNetRawGenerator;

impl ProcFileGenerator for ProcNetRawGenerator {
    fn generate(&self) -> String {
        String::from("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n")
    }
}

// ─── /proc/net/raw6 ─────────────────────────────────────────────────────────
pub struct ProcNetRaw6Generator;

impl ProcFileGenerator for ProcNetRaw6Generator {
    fn generate(&self) -> String {
        String::from("  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n")
    }
}

// ─── /proc/net/packet ───────────────────────────────────────────────────────
pub struct ProcNetPacketGenerator;

impl ProcFileGenerator for ProcNetPacketGenerator {
    fn generate(&self) -> String {
        String::from("sk       RefCnt Type Proto  Iface R Rmem   User   Inode\n")
    }
}

// ─── /proc/net/netlink ──────────────────────────────────────────────────────
pub struct ProcNetNetlinkGenerator;

impl ProcFileGenerator for ProcNetNetlinkGenerator {
    fn generate(&self) -> String {
        String::from("sk        Eth Pid    Groups   Rmem     Wmem     Dump     Locks     Drops     Inode\n")
    }
}

// ─── /proc/net/fib_trie ─────────────────────────────────────────────────────
pub struct ProcNetFibTrieGenerator;

impl ProcFileGenerator for ProcNetFibTrieGenerator {
    fn generate(&self) -> String {
        String::from(
            "Main:\n  +-- 0.0.0.0/0 2 0 2\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     +-- 10.0.2.0/24 2 0 2\n        |-- 10.0.2.0\n           /24 link UNICAST\n        |-- 10.0.2.15\n           /32 host LOCAL\n        |-- 10.0.2.255\n           /32 link BROADCAST\n     +-- 127.0.0.0/8 2 0 2\n        |-- 127.0.0.0\n           /8 host LOCAL\n        |-- 127.0.0.1\n           /32 host LOCAL\n        |-- 127.255.255.255\n           /32 link BROADCAST\nLocal:\n  +-- 0.0.0.0/0 2 0 2\n     +-- 10.0.2.0/24 2 0 2\n        |-- 10.0.2.15\n           /32 host LOCAL\n        |-- 10.0.2.255\n           /32 link BROADCAST\n     +-- 127.0.0.0/8 2 0 2\n        |-- 127.0.0.0\n           /8 host LOCAL\n        |-- 127.0.0.1\n           /32 host LOCAL\n        |-- 127.255.255.255\n           /32 link BROADCAST\n"
        )
    }
}

// ─── /proc/net/fib_trie_stat ────────────────────────────────────────────────
pub struct ProcNetFibTrieStatGenerator;

impl ProcFileGenerator for ProcNetFibTrieStatGenerator {
    fn generate(&self) -> String {
        String::from(
            "Basic info: size of leaf: 48 bytes, size of tnode: 40 bytes.\n\
             Main:\n\
             \tAver depth:     2.00\n\
             \tMax depth:      3\n\
             \tLeaves:         6\n\
             \tPrefixes:       7\n\
             \tInternal nodes: 3\n\
             \t  1: 1  2: 2\n\
             \tPointers: 12\n\
             Counters:\n\
             ---------\n\
             \t          gets = 0\n\
             \t       backtracks = 0\n\
             \tsemantic match passed = 0\n\
             \tsemantic match miss = 0\n\
             \tnull node hit = 0\n\
             \tskipped node resize = 0\n\
             Local:\n\
             \tAver depth:     2.00\n\
             \tMax depth:      3\n\
             \tLeaves:         5\n\
             \tPrefixes:       5\n\
             \tInternal nodes: 2\n\
             \t  1: 1  2: 1\n\
             \tPointers: 8\n\
             Counters:\n\
             ---------\n\
             \t          gets = 0\n\
             \t       backtracks = 0\n\
             \tsemantic match passed = 0\n\
             \tsemantic match miss = 0\n\
             \tnull node hit = 0\n\
             \tskipped node resize = 0\n"
        )
    }
}

// ─── /proc/net/ptype ────────────────────────────────────────────────────────
pub struct ProcNetPtypeGenerator;

impl ProcFileGenerator for ProcNetPtypeGenerator {
    fn generate(&self) -> String {
        String::from("Type Device      Function\n")
    }
}

// ─── /proc/net/wireless ─────────────────────────────────────────────────────
pub struct ProcNetWirelessGenerator;

impl ProcFileGenerator for ProcNetWirelessGenerator {
    fn generate(&self) -> String {
        String::from(
            "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n \
             face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n"
        )
    }
}

// ─── /proc/net/igmp ─────────────────────────────────────────────────────────
pub struct ProcNetIgmpGenerator;

impl ProcFileGenerator for ProcNetIgmpGenerator {
    fn generate(&self) -> String {
        String::from(
            "Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter\n\
             1\tlo        :     1      V3\n\t\t\t\t\t\t01000000\t\t 1 0:00000000\t\t0\n\
             2\teth0      :     1      V3\n\t\t\t\t\t\t01000000\t\t 1 0:00000000\t\t0\n"
        )
    }
}

// ─── /proc/net/igmp6 ────────────────────────────────────────────────────────
pub struct ProcNetIgmp6Generator;

impl ProcFileGenerator for ProcNetIgmp6Generator {
    fn generate(&self) -> String {
        String::from(
            "1    lo              ff020000000000000000000000000001     1 0000000C 0\n\
             2    eth0            ff020000000000000000000000000001     1 0000000C 0\n"
        )
    }
}

// ─── /proc/net/psched ───────────────────────────────────────────────────────
pub struct ProcNetPschedGenerator;

impl ProcFileGenerator for ProcNetPschedGenerator {
    fn generate(&self) -> String {
        String::from("000003e8 3b9aca00 000f4240 3b9aca00\n")
    }
}

// ─── /proc/net/stat/nf_conntrack ────────────────────────────────────────────
// Some LTP tests check /proc/net/nf_conntrack
pub struct ProcNetNfConntrackGenerator;

impl ProcFileGenerator for ProcNetNfConntrackGenerator {
    fn generate(&self) -> String {
        // Empty conntrack table
        String::new()
    }
}

// ─── /proc/net/stat directory (stub) ────────────────────────────────────────
pub struct ProcNetStatNfConntrackGenerator;

impl ProcFileGenerator for ProcNetStatNfConntrackGenerator {
    fn generate(&self) -> String {
        // Header only
        String::from("entries  searched found new invalid ignore delete delete_list insert insert_failed drop early_drop icmp_error  expect_new expect_create expect_delete search_restart\n00000000  00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000  00000000 00000000 00000000 00000000\n")
    }
}

// ─── Helper: Extract IPv4 address from bound_addr ───────────────────────────
fn ip_from_bound(addr: &[u8; 16]) -> u32 {
    // bound_addr layout for AF_INET: [family_le16][port_be16][ip_be32]...
    // bytes 4..8 contain the IPv4 address in network byte order
    u32::from_be_bytes([addr[4], addr[5], addr[6], addr[7]])
}
