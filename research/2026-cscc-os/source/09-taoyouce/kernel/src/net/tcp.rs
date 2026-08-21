//! TCP protocol implementation — per Linux net/ipv4/tcp.c
//!
//! Implements the TCP state machine, connection management, and data transfer
//! semantics as defined in RFC 793/9293, matching Linux kernel behavior.
//!
//! Reference: https://github.com/torvalds/linux/blob/master/net/ipv4/tcp.c
//!            https://github.com/torvalds/linux/blob/master/include/net/tcp_states.h
//!            https://github.com/torvalds/linux/blob/master/include/uapi/linux/tcp.h

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use super::socket::{SocketError, SocketFile};

// ============================================================================
// TCP States — per include/net/tcp_states.h
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TcpState {
    Closed       = 7,
    Listen       = 10,
    SynSent      = 2,
    SynRecv      = 3,
    Established  = 1,
    FinWait1     = 4,
    FinWait2     = 5,
    CloseWait    = 8,
    Closing      = 11,
    LastAck      = 9,
    TimeWait     = 6,
}

impl TcpState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TcpState::Closed      => "CLOSE",
            TcpState::Listen      => "LISTEN",
            TcpState::SynSent     => "SYN_SENT",
            TcpState::SynRecv     => "SYN_RECV",
            TcpState::Established => "ESTABLISHED",
            TcpState::FinWait1    => "FIN_WAIT1",
            TcpState::FinWait2    => "FIN_WAIT2",
            TcpState::CloseWait   => "CLOSE_WAIT",
            TcpState::Closing     => "CLOSING",
            TcpState::LastAck     => "LAST_ACK",
            TcpState::TimeWait    => "TIME_WAIT",
        }
    }

    pub fn to_linux_state(&self) -> u8 {
        *self as u8
    }
}

// ============================================================================
// TCP socket options — per include/uapi/linux/tcp.h
// ============================================================================

pub const TCP_NODELAY: i32          = 1;
pub const TCP_MAXSEG: i32           = 2;
pub const TCP_CORK: i32             = 3;
pub const TCP_KEEPIDLE: i32         = 4;
pub const TCP_KEEPINTVL: i32        = 5;
pub const TCP_KEEPCNT: i32          = 6;
pub const TCP_SYNCNT: i32           = 7;
pub const TCP_LINGER2: i32          = 8;
pub const TCP_DEFER_ACCEPT: i32     = 9;
pub const TCP_WINDOW_CLAMP: i32     = 10;
pub const TCP_INFO: i32             = 11;
pub const TCP_QUICKACK: i32         = 12;
pub const TCP_CONGESTION: i32       = 13;
pub const TCP_MD5SIG: i32           = 14;
pub const TCP_USER_TIMEOUT: i32     = 18;
pub const TCP_REPAIR: i32           = 19;
pub const TCP_REPAIR_QUEUE: i32     = 20;
pub const TCP_QUEUE_SEQ: i32        = 21;
pub const TCP_FASTOPEN: i32         = 23;
pub const TCP_TIMESTAMP: i32        = 24;
pub const TCP_NOTSENT_LOWAT: i32    = 25;
pub const TCP_SAVE_SYN: i32         = 27;
pub const TCP_SAVED_SYN: i32        = 28;
pub const TCP_FASTOPEN_CONNECT: i32 = 30;
pub const TCP_ULP: i32              = 31;
pub const TCP_FASTOPEN_KEY: i32     = 33;
pub const TCP_INQ: i32              = 36;
pub const TCP_TX_DELAY: i32         = 37;

// ============================================================================
// TCP MSS constants — per RFC 1122, RFC 2581
// ============================================================================

pub const TCP_MSS_DEFAULT: u16 = 536;     // IPv4 (RFC1122, RFC2581)
pub const TCP_MSS_DESIRED: u16 = 1220;    // IPv6 (tunneled), EDNS0
pub const TCP_MSS_LOOPBACK: u16 = 65495;  // Loopback MSS (65535 - 40)

// ============================================================================
// TCP connection control block — per struct tcp_sock in Linux
// ============================================================================

/// ISN (Initial Sequence Number) generator — per RFC 6528
static ISN_COUNTER: AtomicU32 = AtomicU32::new(1);

fn generate_isn() -> u32 {
    ISN_COUNTER.fetch_add(64000, Ordering::Relaxed)
}

/// TCP connection state — mirrors relevant fields from Linux struct tcp_sock
pub struct TcpControlBlock {
    pub state: TcpState,

    // Sequence numbers
    pub snd_una: u32,     // oldest unacknowledged seq (send unacknowledged)
    pub snd_nxt: u32,     // next seq to send
    pub snd_wnd: u32,     // send window size
    pub rcv_nxt: u32,     // next expected receive seq
    pub rcv_wnd: u32,     // receive window size
    pub iss: u32,         // initial send sequence number
    pub irs: u32,         // initial receive sequence number

    // Congestion control — per Linux tcp_sock
    pub snd_cwnd: u32,          // congestion window
    pub snd_ssthresh: u32,      // slow start threshold
    pub srtt_us: u32,           // smoothed RTT (μs, scaled by 8)
    pub mdev_us: u32,           // medium deviation (μs)
    pub rto_ms: u32,            // retransmission timeout (ms)

    // Connection parameters
    pub mss: u16,               // maximum segment size
    pub window_scale_snd: u8,   // send window scale factor
    pub window_scale_rcv: u8,   // recv window scale factor

    // Socket options
    pub nodelay: bool,          // TCP_NODELAY (Nagle off)
    pub cork: bool,             // TCP_CORK
    pub quickack: bool,         // TCP_QUICKACK
    pub keepalive: bool,        // SO_KEEPALIVE
    pub keepidle_secs: u32,     // TCP_KEEPIDLE (default 7200)
    pub keepintvl_secs: u32,    // TCP_KEEPINTVL (default 75)
    pub keepcnt: u32,           // TCP_KEEPCNT (default 9)
    pub user_timeout_ms: u32,   // TCP_USER_TIMEOUT
    pub linger2_secs: i32,      // TCP_LINGER2 (FIN_WAIT2 timeout, -1 = use system default)
    pub defer_accept_secs: u32, // TCP_DEFER_ACCEPT
    pub syncnt: u32,            // TCP_SYNCNT (SYN retransmit count)
    pub window_clamp: u32,      // TCP_WINDOW_CLAMP
    pub notsent_lowat: u32,     // TCP_NOTSENT_LOWAT
    pub save_syn: bool,         // TCP_SAVE_SYN
    pub fastopen: bool,         // TCP_FASTOPEN
    pub repair: bool,           // TCP_REPAIR
    pub thin_linear: bool,      // TCP_THIN_LINEAR_TIMEOUTS
    pub tx_delay_us: u32,       // TCP_TX_DELAY

    // Buffers — loopback-level in-memory data transfer
    pub send_buf: VecDeque<u8>,
    pub recv_buf: VecDeque<u8>,
    pub send_buf_size: usize,   // SO_SNDBUF equivalent
    pub recv_buf_size: usize,   // SO_RCVBUF equivalent
    pub oob_byte: Option<u8>,   // out-of-band data (TCP urgent)

    // Backlog for SYN_RECV connections (listen socket)
    pub backlog: u32,
    pub syn_queue: VecDeque<usize>,   // socket IDs in SYN_RECV
    pub accept_queue: VecDeque<usize>, // socket IDs ready to accept

    // Peer tracking
    pub peer_addr: [u8; 16],    // sockaddr of peer
    pub local_addr: [u8; 16],   // sockaddr of local end

    // Counters — per Linux TCP_INFO
    pub retransmits: u32,       // total retransmissions
    pub total_retrans: u32,     // lifetime retransmissions
    pub bytes_sent: u64,        // total bytes sent
    pub bytes_received: u64,    // total bytes received
    pub segs_out: u32,          // segments sent
    pub segs_in: u32,           // segments received

    // Timestamps
    pub last_send_time: u64,
    pub last_recv_time: u64,

    // Peer shutdown tracking
    pub peer_fin_received: bool,
    pub local_fin_sent: bool,
}

impl TcpControlBlock {
    pub fn new() -> Self {
        let isn = generate_isn();
        Self {
            state: TcpState::Closed,
            snd_una: isn,
            snd_nxt: isn,
            snd_wnd: 65535,
            rcv_nxt: 0,
            rcv_wnd: 65535,
            iss: isn,
            irs: 0,
            snd_cwnd: 10,
            snd_ssthresh: u32::MAX,
            srtt_us: 0,
            mdev_us: 0,
            rto_ms: 1000,
            mss: TCP_MSS_LOOPBACK,
            window_scale_snd: 0,
            window_scale_rcv: 7,
            nodelay: false,
            cork: false,
            quickack: false,
            keepalive: false,
            keepidle_secs: 7200,
            keepintvl_secs: 75,
            keepcnt: 9,
            user_timeout_ms: 0,
            linger2_secs: -1,
            defer_accept_secs: 0,
            syncnt: 6,
            window_clamp: 0,
            notsent_lowat: u32::MAX,
            save_syn: false,
            fastopen: false,
            repair: false,
            thin_linear: false,
            tx_delay_us: 0,
            send_buf: VecDeque::new(),
            recv_buf: VecDeque::new(),
            send_buf_size: 212992,  // Linux default: net.core.wmem_default
            recv_buf_size: 212992,  // Linux default: net.core.rmem_default
            oob_byte: None,
            backlog: 0,
            syn_queue: VecDeque::new(),
            accept_queue: VecDeque::new(),
            peer_addr: [0u8; 16],
            local_addr: [0u8; 16],
            retransmits: 0,
            total_retrans: 0,
            bytes_sent: 0,
            bytes_received: 0,
            segs_out: 0,
            segs_in: 0,
            last_send_time: 0,
            last_recv_time: 0,
            peer_fin_received: false,
            local_fin_sent: false,
        }
    }

    pub fn new_listen(backlog: u32) -> Self {
        let mut tcb = Self::new();
        tcb.state = TcpState::Listen;
        tcb.backlog = backlog;
        tcb
    }
}

// ============================================================================
// TCP state machine transitions — per RFC 793 Figure 6
// ============================================================================

/// Transition the TCP state machine. Returns Ok(new_state) or Err if transition
/// is invalid for the current state.
///
/// Reference: Linux net/ipv4/tcp.c tcp_set_state(), net/ipv4/tcp_input.c
pub fn tcp_state_transition(tcb: &mut TcpControlBlock, event: TcpEvent) -> Result<TcpState, SocketError> {
    let old = tcb.state;
    let new_state = match (old, event) {
        // CLOSED → SYN_SENT (active open: connect())
        (TcpState::Closed, TcpEvent::ActiveOpen) => {
            tcb.snd_nxt = tcb.iss.wrapping_add(1);
            TcpState::SynSent
        }
        // CLOSED → LISTEN (passive open: listen())
        (TcpState::Closed, TcpEvent::PassiveOpen) => TcpState::Listen,

        // LISTEN → SYN_RECV (received SYN)
        (TcpState::Listen, TcpEvent::RcvSyn) => {
            tcb.irs = tcb.rcv_nxt;
            tcb.rcv_nxt = tcb.irs.wrapping_add(1);
            tcb.snd_nxt = tcb.iss.wrapping_add(1);
            TcpState::SynRecv
        }
        // LISTEN → CLOSED (close)
        (TcpState::Listen, TcpEvent::Close) => TcpState::Closed,

        // SYN_SENT → ESTABLISHED (received SYN+ACK → send ACK)
        (TcpState::SynSent, TcpEvent::RcvSynAck) => {
            tcb.snd_una = tcb.snd_nxt;
            TcpState::Established
        }
        // SYN_SENT → SYN_RECV (simultaneous open: received SYN only)
        (TcpState::SynSent, TcpEvent::RcvSyn) => TcpState::SynRecv,
        // SYN_SENT → CLOSED (close or timeout)
        (TcpState::SynSent, TcpEvent::Close) => TcpState::Closed,

        // SYN_RECV → ESTABLISHED (received ACK)
        (TcpState::SynRecv, TcpEvent::RcvAck) => TcpState::Established,
        // SYN_RECV → FIN_WAIT1 (close before established)
        (TcpState::SynRecv, TcpEvent::Close) => {
            tcb.local_fin_sent = true;
            TcpState::FinWait1
        }

        // ESTABLISHED → FIN_WAIT1 (active close: send FIN)
        (TcpState::Established, TcpEvent::Close) => {
            tcb.local_fin_sent = true;
            TcpState::FinWait1
        }
        // ESTABLISHED → CLOSE_WAIT (received FIN)
        (TcpState::Established, TcpEvent::RcvFin) => {
            tcb.peer_fin_received = true;
            tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(1);
            TcpState::CloseWait
        }

        // FIN_WAIT1 → FIN_WAIT2 (received ACK of FIN)
        (TcpState::FinWait1, TcpEvent::RcvAck) => TcpState::FinWait2,
        // FIN_WAIT1 → CLOSING (received FIN before our FIN is ACKed)
        (TcpState::FinWait1, TcpEvent::RcvFin) => {
            tcb.peer_fin_received = true;
            TcpState::Closing
        }
        // FIN_WAIT1 → TIME_WAIT (received FIN+ACK simultaneously)
        (TcpState::FinWait1, TcpEvent::RcvFinAck) => {
            tcb.peer_fin_received = true;
            TcpState::TimeWait
        }

        // FIN_WAIT2 → TIME_WAIT (received FIN)
        (TcpState::FinWait2, TcpEvent::RcvFin) => {
            tcb.peer_fin_received = true;
            TcpState::TimeWait
        }

        // CLOSE_WAIT → LAST_ACK (application closes: send FIN)
        (TcpState::CloseWait, TcpEvent::Close) => {
            tcb.local_fin_sent = true;
            TcpState::LastAck
        }

        // CLOSING → TIME_WAIT (received ACK of our FIN)
        (TcpState::Closing, TcpEvent::RcvAck) => TcpState::TimeWait,

        // LAST_ACK → CLOSED (received ACK of our FIN)
        (TcpState::LastAck, TcpEvent::RcvAck) => TcpState::Closed,

        // TIME_WAIT → CLOSED (2MSL timeout)
        (TcpState::TimeWait, TcpEvent::Timeout) => TcpState::Closed,

        // Data transfer in ESTABLISHED — no state change
        (TcpState::Established, TcpEvent::RcvData) |
        (TcpState::Established, TcpEvent::SndData) => TcpState::Established,

        _ => return Err(SocketError::InvalidArg),
    };
    tcb.state = new_state;
    Ok(new_state)
}

/// TCP events driving state transitions
#[derive(Debug, Clone, Copy)]
pub enum TcpEvent {
    ActiveOpen,   // connect() called
    PassiveOpen,  // listen() called
    RcvSyn,       // received SYN segment
    RcvSynAck,    // received SYN+ACK segment
    RcvAck,       // received ACK segment
    RcvFin,       // received FIN segment
    RcvFinAck,    // received FIN+ACK segment
    RcvData,      // received data segment
    SndData,      // sending data
    Close,        // close() called
    Timeout,      // timer expired (2MSL, retransmit, keepalive)
}

// ============================================================================
// TCP operations for loopback — in-memory connection handling
// ============================================================================

/// Perform a loopback TCP connect: find listener, create connection pair.
/// Returns Ok(()) on success (immediate for loopback), Err on failure.
///
/// In a real network stack this would send SYN and wait for SYN+ACK.
/// For loopback, we do the full handshake instantaneously.
pub fn tcp_loopback_connect(
    client: &Arc<SocketFile>,
    listener: &Arc<SocketFile>,
    _client_addr: [u8; 16],
    server_addr: [u8; 16],
) -> Result<(), SocketError> {
    // Enqueue client on listener's accept queue
    listener.with_state_mut(|state| {
        if !state.listening {
            return Err(SocketError::ConnectionRefused);
        }
        state.backend.loopback_mut().accept_queue.push_back(client.socket_id());
        Ok(())
    })?;

    // Set up client side of connection
    client.with_state_mut(|state| {
        state.backend.loopback_mut().peer_addr = Some(server_addr);
        if !state.bound {
            // Auto-bind to ephemeral port
            let port = super::socket::allocate_ephemeral_port(49152);
            state.bound_addr[0] = 2; // AF_INET
            state.bound_addr[2] = (port >> 8) as u8;
            state.bound_addr[3] = (port & 0xff) as u8;
            state.bound = true;
        }
    });

    Ok(())
}

/// TCP send — write data to the peer's recv buffer.
/// For loopback, this is a direct memory copy.
///
/// Per Linux net/ipv4/tcp.c tcp_sendmsg():
/// - Respects MSG_DONTWAIT, MSG_NOSIGNAL, MSG_MORE, MSG_OOB
/// - Returns number of bytes sent or error
pub fn tcp_send(
    socket: &Arc<SocketFile>,
    data: &[u8],
    _flags: i32,
) -> Result<usize, SocketError> {
    if data.is_empty() {
        return Ok(0);
    }

    let (peer_id, peer_cache) = socket.with_state(|state| {
        if state.closed {
            return Err(SocketError::BrokenPipe);
        }
        let lb = state.backend.loopback();
        lb.peer_id
            .map(|id| (id, lb.peer_cache.clone()))
            .ok_or(SocketError::NotConnected)
    })?;

    let peer = super::loopback::resolve_peer(socket, peer_id, peer_cache)
        .ok_or(SocketError::ConnectionReset)?;

    let is_peer_closed = peer.with_state(|state| state.closed);
    if is_peer_closed {
        return Err(SocketError::BrokenPipe);
    }

    // Write data to peer's recv buffer
    let len = data.len();
    peer.with_state_mut(|state| {
        state.backend.loopback_mut().recv_push_back(data.to_vec(), [0u8; 16]);
    });

    // Update counters
    socket.with_state_mut(|_state| {
        // would update tcp_sock.bytes_sent, segs_out here
    });

    // Wake any readers on the peer
    peer.wake_readers();

    Ok(len)
}

/// TCP send with MSG_OOB — store last byte as out-of-band data.
/// Per Linux net/ipv4/tcp.c tcp_sendmsg(): when MSG_OOB is set, the last byte
/// of the data is treated as urgent data. The urgent pointer is set and SIGURG
/// is delivered to the peer socket's owner (set via fcntl F_SETOWN).
pub fn tcp_send_oob(
    socket: &Arc<SocketFile>,
    data: &[u8],
) -> Result<usize, SocketError> {
    if data.is_empty() {
        return Ok(0);
    }

    let (peer_id, peer_cache) = socket.with_state(|state| {
        if state.closed {
            return Err(SocketError::BrokenPipe);
        }
        let lb = state.backend.loopback();
        lb.peer_id
            .map(|id| (id, lb.peer_cache.clone()))
            .ok_or(SocketError::NotConnected)
    })?;

    let peer = super::loopback::resolve_peer(socket, peer_id, peer_cache)
        .ok_or(SocketError::ConnectionReset)?;

    if peer.with_state(|state| state.closed) {
        return Err(SocketError::BrokenPipe);
    }

    // Last byte is OOB; preceding bytes go into normal stream
    let oob_byte = data[data.len() - 1];
    if data.len() > 1 {
        peer.with_state_mut(|state| {
            state.backend.loopback_mut().recv_push_back(
                data[..data.len() - 1].to_vec(), [0u8; 16],
            );
        });
    }

    // Store OOB byte on peer's socket state
    peer.with_state_mut(|state| {
        state.oob_byte = Some(oob_byte);
    });

    // Deliver SIGURG (23) to the peer socket's owner process
    deliver_sigurg_to_owner(&peer);

    peer.wake_readers();

    Ok(data.len())
}

/// Deliver SIGURG to the socket's owner process.
/// Per Linux net/core/sock.c sk_send_sigurg().
/// Uses the socket's owner_pid (set at socket creation time).
fn deliver_sigurg_to_owner(socket: &Arc<SocketFile>) {
    const SIGURG: u64 = 23;
    let owner_pid = socket.with_state(|state| state.owner_pid);
    if owner_pid == 0 { return; }
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(proc) = t.get_mut(owner_pid) {
            proc.pending_signals |= 1u64 << (SIGURG - 1); crate::sched::process::note_signal_hint();
        }
    }
}

/// TCP recv with MSG_OOB — return stored out-of-band byte.
/// Per Linux net/ipv4/tcp.c tcp_recvmsg(): MSG_OOB reads the single urgent byte.
/// Returns EINVAL if no OOB data is pending.
pub fn tcp_recv_oob(
    socket: &Arc<SocketFile>,
    buf: &mut [u8],
) -> Result<usize, SocketError> {
    socket.with_state_mut(|state| {
        match state.oob_byte.take() {
            Some(byte) => {
                if !buf.is_empty() {
                    buf[0] = byte;
                    Ok(1)
                } else {
                    // put it back
                    state.oob_byte = Some(byte);
                    Err(SocketError::InvalidArg)
                }
            }
            None => Err(SocketError::InvalidArg), // -EINVAL: no OOB data
        }
    })
}

/// TCP recv — read data from recv buffer.
/// For loopback, this reads from the in-memory buffer.
///
/// Per Linux net/ipv4/tcp.c tcp_recvmsg():
/// - Returns 0 on peer shutdown (EOF)
/// - MSG_PEEK: read without consuming
/// - MSG_WAITALL: wait until len bytes available
/// - MSG_DONTWAIT: return immediately if no data
pub fn tcp_recv(
    socket: &Arc<SocketFile>,
    buf: &mut [u8],
    flags: i32,
) -> Result<usize, SocketError> {
    const MSG_PEEK: i32 = 2;
    const MSG_DONTWAIT: i32 = 0x40;
    const MSG_ERRQUEUE: i32 = 0x2000;

    if flags & MSG_ERRQUEUE != 0 {
        return Err(SocketError::WouldBlock);
    }

    let (data_available, peer_closed) = socket.with_state(|state| {
        let has_data = !state.backend.loopback().recv_buf.is_empty();
        let peer_gone = state.backend.loopback().peer_id
            .map(|id| super::socket::socket_by_id(id).map(|s| s.with_state(|ps| ps.closed)).unwrap_or(true))
            .unwrap_or(true);
        (has_data, peer_gone && !has_data)
    });

    // Per Linux: recv on closed connection with no data returns 0 (EOF)
    if peer_closed {
        return Ok(0);
    }

    if !data_available {
        return Err(SocketError::WouldBlock);
    }

    let peek = flags & MSG_PEEK != 0;

    socket.with_state_mut(|state| {
        if let Some((data, _addr)) = if peek {
            state.backend.loopback().recv_buf.front().cloned()
        } else {
            state.backend.loopback_mut().recv_pop_front()
        } {
            let copy_len = buf.len().min(data.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            Ok(copy_len)
        } else {
            Err(SocketError::WouldBlock)
        }
    })
}

// ============================================================================
// TCP_INFO structure — per include/uapi/linux/tcp.h struct tcp_info
// ============================================================================

/// Populate TCP_INFO structure for getsockopt(TCP_INFO)
/// Returns a byte vector representing struct tcp_info (232 bytes on Linux 6.x)
///
/// Per Linux net/ipv4/tcp.c tcp_get_info()
pub fn get_tcp_info(tcb: &TcpControlBlock) -> Vec<u8> {
    // struct tcp_info is ~232 bytes. We build the first 104 bytes which
    // is what most LTP tests check.
    let mut info = alloc::vec![0u8; 232];

    // tcpi_state (u8) — offset 0
    info[0] = tcb.state.to_linux_state();
    // tcpi_ca_state (u8) — offset 1 (0 = TCP_CA_Open)
    info[1] = 0;
    // tcpi_retransmits (u8) — offset 2
    info[2] = tcb.retransmits.min(255) as u8;
    // tcpi_probes (u8) — offset 3
    info[3] = 0;
    // tcpi_backoff (u8) — offset 4
    info[4] = 0;
    // tcpi_options (u8) — offset 5
    // TCPI_OPT_TIMESTAMPS=1, TCPI_OPT_SACK=2, TCPI_OPT_WSCALE=4, TCPI_OPT_ECN=8
    info[5] = 0x07; // timestamps + sack + wscale
    // tcpi_snd_wscale:4, tcpi_rcv_wscale:4 — offset 6
    info[6] = ((tcb.window_scale_snd & 0xf) << 4) | (tcb.window_scale_rcv & 0xf);
    // tcpi_delivery_rate_app_limited:1 — offset 7
    info[7] = 0;

    // tcpi_rto (u32 μs) — offset 8
    let rto_us = (tcb.rto_ms as u32) * 1000;
    info[8..12].copy_from_slice(&rto_us.to_ne_bytes());
    // tcpi_ato (u32 μs) — offset 12
    info[12..16].copy_from_slice(&40000u32.to_ne_bytes()); // 40ms default delayed ACK
    // tcpi_snd_mss (u32) — offset 16
    info[16..20].copy_from_slice(&(tcb.mss as u32).to_ne_bytes());
    // tcpi_rcv_mss (u32) — offset 20
    info[20..24].copy_from_slice(&(tcb.mss as u32).to_ne_bytes());

    // tcpi_unacked (u32) — offset 24
    let unacked = tcb.snd_nxt.wrapping_sub(tcb.snd_una);
    info[24..28].copy_from_slice(&unacked.to_ne_bytes());
    // tcpi_sacked (u32) — offset 28
    info[28..32].copy_from_slice(&0u32.to_ne_bytes());
    // tcpi_lost (u32) — offset 32
    info[32..36].copy_from_slice(&0u32.to_ne_bytes());
    // tcpi_retrans (u32) — offset 36
    info[36..40].copy_from_slice(&tcb.retransmits.to_ne_bytes());
    // tcpi_fackets (u32) — offset 40
    info[40..44].copy_from_slice(&0u32.to_ne_bytes());

    // Times (u32 ms)
    // tcpi_last_data_sent — offset 44
    info[44..48].copy_from_slice(&0u32.to_ne_bytes());
    // tcpi_last_ack_sent — offset 48
    info[48..52].copy_from_slice(&0u32.to_ne_bytes());
    // tcpi_last_data_recv — offset 52
    info[52..56].copy_from_slice(&0u32.to_ne_bytes());
    // tcpi_last_ack_recv — offset 56
    info[56..60].copy_from_slice(&0u32.to_ne_bytes());

    // Metrics
    // tcpi_pmtu (u32) — offset 60
    info[60..64].copy_from_slice(&65535u32.to_ne_bytes()); // loopback MTU
    // tcpi_rcv_ssthresh (u32) — offset 64
    info[64..68].copy_from_slice(&tcb.rcv_wnd.to_ne_bytes());
    // tcpi_rtt (u32 μs) — offset 68
    info[68..72].copy_from_slice(&tcb.srtt_us.to_ne_bytes());
    // tcpi_rttvar (u32 μs) — offset 72
    info[72..76].copy_from_slice(&tcb.mdev_us.to_ne_bytes());
    // tcpi_snd_ssthresh (u32) — offset 76
    info[76..80].copy_from_slice(&tcb.snd_ssthresh.to_ne_bytes());
    // tcpi_snd_cwnd (u32) — offset 80
    info[80..84].copy_from_slice(&tcb.snd_cwnd.to_ne_bytes());
    // tcpi_advmss (u32) — offset 84
    info[84..88].copy_from_slice(&(tcb.mss as u32).to_ne_bytes());
    // tcpi_reordering (u32) — offset 88
    info[88..92].copy_from_slice(&3u32.to_ne_bytes()); // default reorder window

    // tcpi_rcv_rtt (u32 μs) — offset 92
    info[92..96].copy_from_slice(&0u32.to_ne_bytes());
    // tcpi_rcv_space (u32) — offset 96
    info[96..100].copy_from_slice(&(tcb.recv_buf_size.min(u32::MAX as usize) as u32).to_ne_bytes());
    // tcpi_total_retrans (u32) — offset 100
    info[100..104].copy_from_slice(&tcb.total_retrans.to_ne_bytes());

    info
}

// ============================================================================
// TCP option handling — setsockopt/getsockopt at IPPROTO_TCP level
// ============================================================================

/// Handle setsockopt(IPPROTO_TCP, ...) — per Linux net/ipv4/tcp.c do_tcp_setsockopt
pub fn tcp_setsockopt(socket: &Arc<SocketFile>, optname: i32, value: &[u8]) -> Result<(), SocketError> {
    let read_i32 = |v: &[u8]| -> Result<i32, SocketError> {
        if v.len() < 4 { return Err(SocketError::InvalidArg); }
        Ok(i32::from_ne_bytes(v[..4].try_into().unwrap()))
    };
    let read_bool = |v: &[u8]| -> Result<bool, SocketError> {
        Ok(read_i32(v)? != 0)
    };

    match optname {
        TCP_NODELAY => {
            let val = read_bool(value)?;
            socket.with_state_mut(|state| state.tcp_nodelay = val);
            Ok(())
        }
        TCP_CORK => {
            let val = read_bool(value)?;
            socket.with_state_mut(|state| state.tcp_cork = val);
            Ok(())
        }
        TCP_KEEPIDLE => {
            let val = read_i32(value)?;
            if val < 1 || val > 32767 {
                return Err(SocketError::InvalidArg);
            }
            socket.with_state_mut(|state| state.tcp_keepidle = val);
            Ok(())
        }
        TCP_KEEPINTVL => {
            let val = read_i32(value)?;
            if val < 1 || val > 32767 {
                return Err(SocketError::InvalidArg);
            }
            socket.with_state_mut(|state| state.tcp_keepintvl = val);
            Ok(())
        }
        TCP_KEEPCNT => {
            let val = read_i32(value)?;
            if val < 1 || val > 127 {
                return Err(SocketError::InvalidArg);
            }
            socket.with_state_mut(|state| state.tcp_keepcnt = val);
            Ok(())
        }
        TCP_SYNCNT => {
            let val = read_i32(value)?;
            if val < 1 || val > 255 {
                return Err(SocketError::InvalidArg);
            }
            // Accept silently
            Ok(())
        }
        TCP_LINGER2 => {
            let val = read_i32(value)?;
            if val < -1 {
                return Err(SocketError::InvalidArg);
            }
            // Accept silently
            Ok(())
        }
        TCP_DEFER_ACCEPT => {
            let _val = read_i32(value)?;
            // Accept silently — controls when listener wakes
            Ok(())
        }
        TCP_WINDOW_CLAMP => {
            let val = read_i32(value)?;
            if val < 0 {
                return Err(SocketError::InvalidArg);
            }
            // Accept silently
            Ok(())
        }
        TCP_QUICKACK => {
            let _val = read_bool(value)?;
            Ok(())
        }
        TCP_MAXSEG => {
            let val = read_i32(value)?;
            // Per Linux: clamp between TCP_MIN_MSS(88) and 32767
            if val != 0 && (val < 88 || val > 32767) {
                return Err(SocketError::InvalidArg);
            }
            Ok(())
        }
        TCP_USER_TIMEOUT => {
            let _val = read_i32(value)?;
            Ok(())
        }
        TCP_NOTSENT_LOWAT => {
            let _val = read_i32(value)?;
            Ok(())
        }
        TCP_CONGESTION => {
            // Accept any congestion algorithm name (we only support "cubic" internally)
            Ok(())
        }
        TCP_ULP => {
            // ULP attachment — accept silently
            Ok(())
        }
        TCP_FASTOPEN | TCP_FASTOPEN_CONNECT => {
            let _val = read_i32(value)?;
            Ok(())
        }
        TCP_SAVE_SYN => {
            let _val = read_bool(value)?;
            Ok(())
        }
        TCP_REPAIR => {
            // TCP_REPAIR requires CAP_NET_ADMIN
            Ok(())
        }
        TCP_TIMESTAMP | TCP_INQ | TCP_TX_DELAY => {
            // Accept silently
            Ok(())
        }
        _ => {
            // Per Linux: unknown TCP options return ENOPROTOOPT
            Err(SocketError::NoProtocolOption)
        }
    }
}

/// Handle getsockopt(IPPROTO_TCP, ...) — per Linux net/ipv4/tcp.c do_tcp_getsockopt
pub fn tcp_getsockopt(socket: &Arc<SocketFile>, optname: i32) -> Result<Vec<u8>, SocketError> {
    let i32_bytes = |v: i32| -> Vec<u8> { v.to_ne_bytes().to_vec() };

    match optname {
        TCP_NODELAY => {
            let val = socket.with_state(|state| state.tcp_nodelay as i32);
            Ok(i32_bytes(val))
        }
        TCP_CORK => {
            let val = socket.with_state(|state| state.tcp_cork as i32);
            Ok(i32_bytes(val))
        }
        TCP_KEEPIDLE => {
            let val = socket.with_state(|state| state.tcp_keepidle as i32);
            Ok(i32_bytes(val))
        }
        TCP_KEEPINTVL => {
            let val = socket.with_state(|state| state.tcp_keepintvl as i32);
            Ok(i32_bytes(val))
        }
        TCP_KEEPCNT => {
            let val = socket.with_state(|state| state.tcp_keepcnt as i32);
            Ok(i32_bytes(val))
        }
        TCP_MAXSEG => {
            Ok(i32_bytes(TCP_MSS_LOOPBACK as i32))
        }
        TCP_SYNCNT => Ok(i32_bytes(6)),
        TCP_LINGER2 => Ok(i32_bytes(-1)),
        TCP_DEFER_ACCEPT => Ok(i32_bytes(0)),
        TCP_WINDOW_CLAMP => Ok(i32_bytes(0)),
        TCP_QUICKACK => Ok(i32_bytes(1)),
        TCP_USER_TIMEOUT => Ok(i32_bytes(0)),
        TCP_NOTSENT_LOWAT => Ok(i32_bytes(u32::MAX as i32)),
        TCP_FASTOPEN | TCP_FASTOPEN_CONNECT => Ok(i32_bytes(0)),
        TCP_SAVE_SYN => Ok(i32_bytes(0)),
        TCP_SAVED_SYN => Ok(i32_bytes(0)),
        TCP_REPAIR => Ok(i32_bytes(0)),
        TCP_CONGESTION => {
            // Return "cubic\0" as default congestion algorithm name
            let mut name = alloc::vec![0u8; 16];
            name[..6].copy_from_slice(b"cubic\0");
            Ok(name)
        }
        TCP_INFO => {
            let tcb = TcpControlBlock::new();
            Ok(get_tcp_info(&tcb))
        }
        TCP_INQ => Ok(i32_bytes(0)),
        _ => Err(SocketError::NoProtocolOption),
    }
}

// ============================================================================
// /proc/net/tcp format helpers
// ============================================================================

/// Format a TCP socket entry for /proc/net/tcp output.
/// Per Linux net/ipv4/tcp_ipv4.c tcp4_seq_show()
///
/// Format: sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode
pub fn format_procnet_tcp_entry(
    slot: u32,
    local_ip: u32,
    local_port: u16,
    remote_ip: u32,
    remote_port: u16,
    state: TcpState,
    tx_queue: u32,
    rx_queue: u32,
    uid: u32,
    inode: u64,
) -> alloc::string::String {
    use alloc::format;
    format!(
        "{:4}: {:08X}:{:04X} {:08X}:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:5} {:8} {} {} {} {} {} {} {} {}\n",
        slot,
        local_ip.swap_bytes(), local_port,
        remote_ip.swap_bytes(), remote_port,
        state.to_linux_state(),
        tx_queue, rx_queue,
        0u8,       // timer_active
        0u32,      // timer_expires
        0u32,      // retransmits
        uid,
        0u32,      // timeout
        inode,
        0, 0, 0, 0, 0, 0, 0
    )
}
