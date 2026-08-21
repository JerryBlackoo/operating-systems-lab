//! TCP Congestion Control — per Linux net/ipv4/tcp_cong.c
//!
//! Implements pluggable congestion control algorithms matching Linux semantics:
//! - TCP Reno (NewReno): RFC 5681 slow start + congestion avoidance + fast recovery
//! - TCP CUBIC: RFC 8312 cubic function growth (Linux default since 2.6.19)
//!
//! Reference:
//! - https://github.com/torvalds/linux/blob/master/net/ipv4/tcp_cong.c
//! - https://github.com/torvalds/linux/blob/master/net/ipv4/tcp_cubic.c
//! - https://github.com/torvalds/linux/blob/master/net/ipv4/tcp_input.c

use core::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// Congestion Algorithm Trait — per Linux struct tcp_congestion_ops
// ============================================================================

/// Congestion control algorithm identifier
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CongAlgorithm {
    Reno,
    Cubic,
}

impl CongAlgorithm {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Reno => "reno",
            Self::Cubic => "cubic",
        }
    }

    pub fn from_name(name: &[u8]) -> Option<Self> {
        let s = core::str::from_utf8(name).ok()?;
        let s = s.trim_end_matches('\0').trim();
        match s {
            "reno" | "newreno" => Some(Self::Reno),
            "cubic" => Some(Self::Cubic),
            _ => None,
        }
    }
}

// ============================================================================
// Congestion State — per Linux tcp_sock congestion-related fields
// ============================================================================

/// Congestion Avoidance state machine (per Linux enum tcp_ca_state)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum CaState {
    Open = 0,       // Normal state, no loss detected
    Disorder = 1,   // Some dupacks received
    Cwr = 2,        // Congestion window reduced (ECN)
    Recovery = 3,   // Fast recovery (3 dupacks)
    Loss = 4,       // RTO timeout triggered
}

/// Per-connection congestion control state
pub struct CongState {
    pub algorithm: CongAlgorithm,
    pub ca_state: CaState,

    // Core Reno variables
    pub snd_cwnd: u32,           // congestion window (segments)
    pub snd_ssthresh: u32,       // slow start threshold (segments)
    pub snd_cwnd_cnt: u32,       // linear increase counter (for CA phase)
    pub snd_cwnd_clamp: u32,     // max cwnd allowed

    // RTT estimation (per Jacobson/Karels)
    pub srtt_us: u32,            // smoothed RTT in μs (scaled <<3)
    pub mdev_us: u32,            // mean deviation (μs)
    pub mdev_max_us: u32,        // max deviation in current RTT
    pub rttvar_us: u32,          // smoothed mdev (μs, scaled <<2)
    pub rto_ms: u32,             // retransmission timeout (ms)

    // Recovery tracking
    pub prior_cwnd: u32,         // cwnd before entering recovery
    pub prior_ssthresh: u32,     // ssthresh before entering recovery
    pub high_seq: u32,           // highest seq when entering recovery
    pub dup_ack_count: u32,      // consecutive duplicate ACKs

    // CUBIC-specific state
    pub cubic: CubicState,

    // Counters
    pub bytes_acked: u64,        // total bytes acknowledged
    pub pkts_acked: u32,         // packets acknowledged (for cwnd adjustment)
}

/// CUBIC algorithm state — per Linux struct bictcp
pub struct CubicState {
    pub cnt: u32,                // cwnd increase count per RTT
    pub last_cwnd: u32,          // last max cwnd (Wmax)
    pub last_time: u64,          // time of last congestion event (ms)
    pub epoch_start: u64,        // beginning of current epoch (ms)
    pub origin_point: u32,       // Wmax at start of epoch
    pub k: u32,                  // time to reach Wmax from origin (scaled)
    pub tcp_cwnd: u32,           // estimated Reno cwnd for TCP-friendliness
    pub ack_cnt: u32,            // ACK count since last cwnd change
    pub found_slow_start: bool,  // did we exit slow start?
}

// CUBIC parameters — per Linux tcp_cubic.c
const CUBIC_C: u32 = 410;       // C constant (0.4 * 1024, scaled)
const CUBIC_BETA: u32 = 717;    // β = 0.7 (717/1024)
const CUBIC_BETA_SCALE: u32 = 1024;

// Global monotonic clock for congestion timing (ms since boot)
static CONG_CLOCK_MS: AtomicU64 = AtomicU64::new(0);

/// Called from timer tick to advance congestion clock
pub fn cong_clock_tick(ms: u64) {
    CONG_CLOCK_MS.fetch_add(ms, Ordering::Relaxed);
}

pub fn cong_clock_now() -> u64 {
    CONG_CLOCK_MS.load(Ordering::Relaxed)
}

impl CubicState {
    pub fn new() -> Self {
        Self {
            cnt: 0,
            last_cwnd: 0,
            last_time: 0,
            epoch_start: 0,
            origin_point: 0,
            k: 0,
            tcp_cwnd: 0,
            ack_cnt: 0,
            found_slow_start: false,
        }
    }

    /// Reset CUBIC state on loss event
    pub fn reset(&mut self) {
        self.epoch_start = 0;
        self.origin_point = 0;
        self.k = 0;
        self.ack_cnt = 0;
    }
}

impl CongState {
    pub fn new(algorithm: CongAlgorithm) -> Self {
        Self {
            algorithm,
            ca_state: CaState::Open,
            snd_cwnd: 10,              // IW10 per RFC 6928
            snd_ssthresh: u32::MAX,    // infinity until first loss
            snd_cwnd_cnt: 0,
            snd_cwnd_clamp: u32::MAX,
            srtt_us: 0,
            mdev_us: 0,
            mdev_max_us: 0,
            rttvar_us: 0,
            rto_ms: 1000,              // initial RTO = 1s (RFC 6298)
            prior_cwnd: 0,
            prior_ssthresh: 0,
            high_seq: 0,
            dup_ack_count: 0,
            cubic: CubicState::new(),
            bytes_acked: 0,
            pkts_acked: 0,
        }
    }

    // ========================================================================
    // RTT measurement — per Linux tcp_rtt_estimator() (net/ipv4/tcp_input.c)
    // ========================================================================

    /// Update RTT estimate per Jacobson/Karels algorithm (RFC 6298)
    pub fn rtt_update(&mut self, rtt_us: u32) {
        if self.srtt_us == 0 {
            // First measurement
            self.srtt_us = rtt_us << 3;        // srtt = rtt * 8
            self.mdev_us = rtt_us << 1;        // mdev = rtt / 2 (scaled by 4 = rtt*2)
            self.mdev_max_us = rtt_us << 1;
            self.rttvar_us = rtt_us << 1;      // rttvar = rtt / 2 (scaled by 4)
        } else {
            // EWMA update
            let delta = if rtt_us > (self.srtt_us >> 3) {
                rtt_us - (self.srtt_us >> 3)
            } else {
                (self.srtt_us >> 3) - rtt_us
            };
            // mdev = (3/4)*mdev + (1/4)*|delta|
            self.mdev_us = ((3 * self.mdev_us) + delta) >> 2;
            if self.mdev_us > self.mdev_max_us {
                self.mdev_max_us = self.mdev_us;
                if self.mdev_max_us > self.rttvar_us {
                    self.rttvar_us = self.mdev_max_us;
                }
            }
            // srtt = (7/8)*srtt + (1/8)*rtt
            self.srtt_us = self.srtt_us - (self.srtt_us >> 3) + rtt_us;
        }
        // RTO = srtt + 4*rttvar, minimum 200ms per Linux (TCP_RTO_MIN)
        let rto_us = (self.srtt_us >> 3) + self.rttvar_us;
        self.rto_ms = (rto_us / 1000).max(200).min(120_000); // min 200ms, max 120s
    }

    // ========================================================================
    // ACK processing — per Linux tcp_ack() → tcp_cong_avoid()
    // ========================================================================

    /// Process a new ACK (non-duplicate). Called when snd_una advances.
    /// `acked_bytes`: number of newly acknowledged bytes
    /// `mss`: current MSS for segment counting
    pub fn on_ack(&mut self, acked_bytes: u32, mss: u32) {
        let acked_segs = (acked_bytes + mss - 1) / mss; // round up to segments
        self.bytes_acked += acked_bytes as u64;
        self.pkts_acked += acked_segs;
        self.dup_ack_count = 0;

        if self.ca_state == CaState::Recovery {
            // In recovery: deflate window (NewReno)
            // Per RFC 6675: during recovery, cwnd = ssthresh for non-SACK
            return;
        }

        match self.algorithm {
            CongAlgorithm::Reno => self.reno_cong_avoid(acked_segs, mss),
            CongAlgorithm::Cubic => self.cubic_cong_avoid(acked_segs, mss),
        }

        // Clamp cwnd
        if self.snd_cwnd > self.snd_cwnd_clamp {
            self.snd_cwnd = self.snd_cwnd_clamp;
        }
    }

    /// Process a duplicate ACK. Called when we receive an ACK that doesn't advance snd_una.
    pub fn on_dup_ack(&mut self, snd_nxt: u32) {
        self.dup_ack_count += 1;

        if self.dup_ack_count == 3 && self.ca_state == CaState::Open {
            // Enter fast recovery (RFC 5681 §3.2)
            self.enter_recovery(snd_nxt);
        } else if self.ca_state == CaState::Recovery {
            // Inflate cwnd by 1 segment per dupack during recovery
            self.snd_cwnd += 1;
        }
    }

    /// Enter fast recovery state
    fn enter_recovery(&mut self, snd_nxt: u32) {
        self.ca_state = CaState::Recovery;
        self.high_seq = snd_nxt;
        self.prior_cwnd = self.snd_cwnd;
        self.prior_ssthresh = self.snd_ssthresh;

        match self.algorithm {
            CongAlgorithm::Reno => {
                // Reno: ssthresh = cwnd/2, cwnd = ssthresh + 3 (RFC 5681)
                self.snd_ssthresh = (self.snd_cwnd / 2).max(2);
                self.snd_cwnd = self.snd_ssthresh + 3;
            }
            CongAlgorithm::Cubic => {
                // CUBIC: ssthresh = cwnd * beta, cwnd = ssthresh + 3
                self.snd_ssthresh = ((self.snd_cwnd as u64 * CUBIC_BETA as u64)
                    / CUBIC_BETA_SCALE as u64).max(2) as u32;
                self.snd_cwnd = self.snd_ssthresh + 3;
                // Record last max for CUBIC curve
                self.cubic.last_cwnd = self.prior_cwnd;
                self.cubic.last_time = cong_clock_now();
                self.cubic.reset();
            }
        }
    }

    /// Exit fast recovery when all data up to high_seq is acknowledged
    pub fn exit_recovery(&mut self) {
        self.ca_state = CaState::Open;
        self.dup_ack_count = 0;

        match self.algorithm {
            CongAlgorithm::Reno => {
                // Deflate: cwnd = ssthresh (RFC 5681 §3.2 step 3)
                self.snd_cwnd = self.snd_ssthresh;
            }
            CongAlgorithm::Cubic => {
                self.snd_cwnd = self.snd_ssthresh;
            }
        }
        self.snd_cwnd_cnt = 0;
    }

    /// Handle RTO (retransmission timeout) — per Linux tcp_enter_loss()
    pub fn on_timeout(&mut self) {
        self.ca_state = CaState::Loss;
        self.prior_cwnd = self.snd_cwnd;

        match self.algorithm {
            CongAlgorithm::Reno => {
                // Reno: ssthresh = max(cwnd/2, 2), cwnd = 1
                self.snd_ssthresh = (self.snd_cwnd / 2).max(2);
            }
            CongAlgorithm::Cubic => {
                // CUBIC: ssthresh = cwnd * beta
                self.snd_ssthresh = ((self.snd_cwnd as u64 * CUBIC_BETA as u64)
                    / CUBIC_BETA_SCALE as u64).max(2) as u32;
                self.cubic.last_cwnd = self.snd_cwnd;
                self.cubic.last_time = cong_clock_now();
                self.cubic.reset();
            }
        }
        // Reset cwnd to 1 segment (back to slow start)
        self.snd_cwnd = 1;
        self.snd_cwnd_cnt = 0;
        self.dup_ack_count = 0;
    }

    /// Exit loss state when recovery completes
    pub fn exit_loss(&mut self) {
        self.ca_state = CaState::Open;
        self.snd_cwnd_cnt = 0;
    }

    // ========================================================================
    // Reno congestion avoidance — per Linux tcp_reno_cong_avoid()
    // ========================================================================

    fn reno_cong_avoid(&mut self, acked_segs: u32, _mss: u32) {
        if self.snd_cwnd <= self.snd_ssthresh {
            // Slow start: cwnd += acked_segs (exponential growth)
            self.snd_cwnd += acked_segs;
        } else {
            // Congestion avoidance: cwnd += 1/cwnd per ACK (linear growth)
            // Accumulate and increment when count reaches cwnd
            self.snd_cwnd_cnt += acked_segs;
            if self.snd_cwnd_cnt >= self.snd_cwnd {
                self.snd_cwnd_cnt -= self.snd_cwnd;
                self.snd_cwnd += 1;
            }
        }
    }

    // ========================================================================
    // CUBIC congestion avoidance — per Linux bictcp_cong_avoid()
    // ========================================================================

    fn cubic_cong_avoid(&mut self, acked_segs: u32, _mss: u32) {
        if self.snd_cwnd <= self.snd_ssthresh {
            // Slow start: exponential growth (same as Reno)
            self.snd_cwnd += acked_segs;
            self.cubic.found_slow_start = true;
            return;
        }

        let now = cong_clock_now();

        // Initialize epoch
        if self.cubic.epoch_start == 0 {
            self.cubic.epoch_start = now;
            if self.snd_cwnd < self.cubic.last_cwnd {
                // W_max from last epoch
                self.cubic.origin_point = self.cubic.last_cwnd;
                // K = cubic_root((Wmax - cwnd) / C)
                let diff = self.cubic.origin_point - self.snd_cwnd;
                self.cubic.k = cubic_root(diff * CUBIC_BETA_SCALE / CUBIC_C);
            } else {
                self.cubic.origin_point = self.snd_cwnd;
                self.cubic.k = 0;
            }
            self.cubic.ack_cnt = 1;
            self.cubic.tcp_cwnd = self.snd_cwnd;
        }

        // Time since epoch start (ms)
        let t = now.saturating_sub(self.cubic.epoch_start) as u32;

        // CUBIC window: W(t) = C*(t-K)^3 + Wmax
        let target = cubic_target(t, self.cubic.k, self.cubic.origin_point);

        // TCP-friendliness: ensure we grow at least as fast as Reno
        self.cubic.tcp_cwnd += (acked_segs * 3) / (8 * self.cubic.tcp_cwnd.max(1));
        let tcp_target = self.cubic.tcp_cwnd;

        let final_target = target.max(tcp_target);

        if final_target > self.snd_cwnd {
            // Calculate how many ACKs needed to increase cwnd by 1
            let diff = final_target - self.snd_cwnd;
            self.cubic.cnt = self.snd_cwnd / diff.max(1);
        } else {
            self.cubic.cnt = 100; // very slow growth when above target
        }

        // Apply growth
        self.cubic.ack_cnt += acked_segs;
        if self.cubic.cnt > 0 && self.cubic.ack_cnt >= self.cubic.cnt {
            self.snd_cwnd += 1;
            self.cubic.ack_cnt = 0;
        }
    }
}

// ============================================================================
// CUBIC math helpers
// ============================================================================

/// Integer cube root approximation (per Linux cubic_root in tcp_cubic.c)
fn cubic_root(x: u32) -> u32 {
    if x == 0 { return 0; }
    // Newton's method: start with rough estimate
    let mut y = 1u32;
    // Find initial estimate using bit manipulation
    let bits = 32 - x.leading_zeros();
    y = 1u32 << ((bits + 2) / 3);
    // 3 iterations of Newton's method: y = (2*y + x/(y*y)) / 3
    for _ in 0..4 {
        let y2 = (y as u64) * (y as u64);
        if y2 == 0 { break; }
        y = ((2 * y as u64 + x as u64 / y2) / 3) as u32;
        if y == 0 { y = 1; break; }
    }
    y
}

/// Compute CUBIC target window: W(t) = C*(t-K)^3 + Wmax
/// t and K in ms, result in segments
fn cubic_target(t_ms: u32, k_ms: u32, origin: u32) -> u32 {
    // (t - K) in ms; can be negative (approaching from below)
    let dt = t_ms as i64 - k_ms as i64;
    // C * (t-K)^3 — scale: C is in 1/1024, time in ms
    // We compute in segments: C/1024 * (dt_ms/1000)^3 + origin
    // = C * dt^3 / (1024 * 10^9) + origin
    let dt_cubed = dt * dt * dt; // can be large, use i64
    // Scale: CUBIC_C=410 ≈ 0.4*1024
    let increment = (CUBIC_C as i64 * dt_cubed) / (1024 * 1_000_000_000i64);
    let result = origin as i64 + increment;
    result.max(1) as u32
}

// ============================================================================
// Integration helpers — called from TCP send/recv paths
// ============================================================================

/// Sync congestion state back to TcpControlBlock fields for TCP_INFO reporting
pub fn sync_to_tcb(cong: &CongState, tcb: &mut super::tcp::TcpControlBlock) {
    tcb.snd_cwnd = cong.snd_cwnd;
    tcb.snd_ssthresh = cong.snd_ssthresh;
    tcb.srtt_us = cong.srtt_us >> 3; // un-scale for reporting
    tcb.mdev_us = cong.mdev_us;
    tcb.rto_ms = cong.rto_ms;
}
