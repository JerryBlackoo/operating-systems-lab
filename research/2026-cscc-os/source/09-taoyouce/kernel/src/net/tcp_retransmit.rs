//! TCP Retransmission — per Linux net/ipv4/tcp_output.c, net/ipv4/tcp_input.c
//!
//! Implements:
//! - Retransmission queue: holds unacknowledged segments for potential retransmit
//! - RTO-based retransmission: exponential backoff on timeout
//! - Fast retransmit: triggered by 3 duplicate ACKs (RFC 5681)
//! - SACK (Selective Acknowledgment): RFC 2018, RFC 3517
//!   - SACK option parsing and scoreboard tracking
//!   - SACK-based loss detection
//!   - Selective retransmission of lost segments
//!
//! Reference:
//! - https://github.com/torvalds/linux/blob/master/net/ipv4/tcp_output.c
//! - https://github.com/torvalds/linux/blob/master/net/ipv4/tcp_input.c
//! - https://github.com/torvalds/linux/blob/master/include/net/tcp.h (tcp_skb_cb)

use alloc::collections::VecDeque;
use alloc::vec::Vec;

// ============================================================================
// Retransmit Queue Entry — per Linux struct sk_buff / tcp_skb_cb
// ============================================================================

/// A segment held in the retransmission queue awaiting acknowledgment
#[derive(Clone)]
pub struct RetransmitEntry {
    /// Start sequence number of this segment
    pub seq: u32,
    /// End sequence number (seq + payload len; FIN/SYN count as 1 byte)
    pub end_seq: u32,
    /// Segment payload (data bytes only, no TCP header)
    pub data: Vec<u8>,
    /// Transmission timestamp (ms, for RTT calculation)
    pub tx_time_ms: u64,
    /// Number of times this segment has been (re)transmitted
    pub transmit_count: u32,
    /// Whether this segment has been SACKed (acknowledged out of order)
    pub sacked: bool,
    /// Whether this segment is considered lost
    pub lost: bool,
}

// ============================================================================
// SACK Scoreboard — per Linux struct tcp_sack_block / tcp_sacktag_state
// ============================================================================

/// A single SACK block (left_edge, right_edge) — per RFC 2018
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SackBlock {
    pub left: u32,   // first sequence number in block
    pub right: u32,  // sequence number after last in block
}

impl SackBlock {
    pub fn contains_seq(&self, seq: u32) -> bool {
        // Handle sequence number wrap-around
        seq_between(self.left, seq, self.right)
    }

    pub fn len(&self) -> u32 {
        self.right.wrapping_sub(self.left)
    }
}

/// SACK scoreboard — tracks which segments have been selectively acknowledged
pub struct SackScoreboard {
    /// Received SACK blocks (up to 4 per TCP option, we store up to 32 merged)
    pub blocks: Vec<SackBlock>,
    /// Number of segments marked as lost based on SACK information
    pub lost_count: u32,
    /// Number of SACKed segments (not yet ACKed by cumulative ACK)
    pub sacked_count: u32,
    /// Highest SACKed sequence number
    pub fack: u32,
}

impl SackScoreboard {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            lost_count: 0,
            sacked_count: 0,
            fack: 0,
        }
    }

    /// Process incoming SACK blocks from a received ACK segment.
    /// Per Linux tcp_sacktag_write_queue()
    pub fn update(&mut self, new_blocks: &[SackBlock], snd_una: u32) {
        for &block in new_blocks {
            // Validate: block must be within [snd_una, snd_nxt]
            if !seq_after(block.right, snd_una) {
                continue; // stale block
            }
            self.insert_block(block);
            // Update FACK (forward ACK) — highest right edge
            if seq_after(block.right, self.fack) {
                self.fack = block.right;
            }
        }
        // Prune blocks that are now covered by cumulative ACK
        self.blocks.retain(|b| seq_after(b.right, snd_una));
    }

    /// Insert a new SACK block, merging with existing overlapping blocks
    fn insert_block(&mut self, new: SackBlock) {
        // Merge with overlapping/adjacent blocks
        let mut merged = new;
        self.blocks.retain(|existing| {
            if blocks_overlap_or_adjacent(existing, &merged) {
                // Extend merged to cover both
                if seq_before(existing.left, merged.left) {
                    merged.left = existing.left;
                }
                if seq_after(existing.right, merged.right) {
                    merged.right = existing.right;
                }
                false // remove old block (merged into new)
            } else {
                true
            }
        });
        self.blocks.push(merged);
        // Keep at most 32 blocks
        if self.blocks.len() > 32 {
            self.blocks.remove(0);
        }
    }

    /// Check if a sequence number has been SACKed
    pub fn is_sacked(&self, seq: u32) -> bool {
        self.blocks.iter().any(|b| b.contains_seq(seq))
    }

    /// Clear all state (on RTO or connection reset)
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.lost_count = 0;
        self.sacked_count = 0;
        self.fack = 0;
    }
}

// ============================================================================
// Retransmission Queue — per Linux tcp_write_queue
// ============================================================================

pub struct RetransmitQueue {
    /// Ordered queue of unACKed segments
    pub queue: VecDeque<RetransmitEntry>,
    /// SACK scoreboard
    pub sack: SackScoreboard,
    /// Number of retransmissions in current loss episode
    pub retrans_out: u32,
    /// Total retransmissions since connection start
    pub total_retrans: u32,
    /// Whether SACK is enabled for this connection (negotiated in SYN)
    pub sack_enabled: bool,
    /// DupThresh: number of duplicate ACKs / SACK blocks to declare loss (RFC 6675: 3)
    pub reorder_threshold: u32,
}

impl RetransmitQueue {
    pub fn new(sack_enabled: bool) -> Self {
        Self {
            queue: VecDeque::new(),
            sack: SackScoreboard::new(),
            retrans_out: 0,
            total_retrans: 0,
            sack_enabled,
            reorder_threshold: 3, // dupthresh = 3 (RFC 5681, RFC 6675)
        }
    }

    /// Add a newly transmitted segment to the retransmit queue
    pub fn enqueue(&mut self, seq: u32, data: &[u8], tx_time_ms: u64) {
        let end_seq = seq.wrapping_add(data.len() as u32);
        self.queue.push_back(RetransmitEntry {
            seq,
            end_seq,
            data: data.to_vec(),
            tx_time_ms,
            transmit_count: 1,
            sacked: false,
            lost: false,
        });
    }

    /// Process cumulative ACK: remove all segments with end_seq <= ack_seq
    /// Returns (bytes_acked, rtt_sample_us) where rtt_sample is from the
    /// oldest non-retransmitted segment (Karn's algorithm)
    pub fn on_ack(&mut self, ack_seq: u32, now_ms: u64) -> (u32, Option<u32>) {
        let mut bytes_acked = 0u32;
        let mut rtt_sample = None;

        while let Some(front) = self.queue.front() {
            if !seq_after(front.end_seq, ack_seq) || front.end_seq == ack_seq {
                let entry = self.queue.pop_front().unwrap();
                let seg_len = entry.end_seq.wrapping_sub(entry.seq);
                bytes_acked += seg_len;
                // Karn's algorithm: only use RTT sample from non-retransmitted segments
                if entry.transmit_count == 1 && rtt_sample.is_none() {
                    let rtt_ms = now_ms.saturating_sub(entry.tx_time_ms);
                    rtt_sample = Some((rtt_ms * 1000) as u32); // convert to μs
                }
            } else {
                break;
            }
        }

        // Update SACK scoreboard: prune blocks below new snd_una
        self.sack.blocks.retain(|b| seq_after(b.right, ack_seq));

        (bytes_acked, rtt_sample)
    }

    /// Process SACK blocks received in an ACK
    /// Returns segments newly marked as lost (for retransmission)
    pub fn on_sack(&mut self, blocks: &[SackBlock], snd_una: u32) -> Vec<u32> {
        if !self.sack_enabled || blocks.is_empty() {
            return Vec::new();
        }

        self.sack.update(blocks, snd_una);

        // Mark segments in queue as sacked/lost based on scoreboard
        let mut sacked_count = 0u32;
        let mut lost_seqs = Vec::new();

        for entry in self.queue.iter_mut() {
            if self.sack.is_sacked(entry.seq) {
                if !entry.sacked {
                    entry.sacked = true;
                    sacked_count += 1;
                }
            }
        }
        self.sack.sacked_count = sacked_count;

        // SACK-based loss detection (RFC 6675 §4):
        // A segment is considered lost if it has at least `reorder_threshold`
        // SACKed segments above it.
        // Collect sacked sequence numbers first to avoid borrow conflict.
        let sacked_seqs: Vec<u32> = self.queue.iter()
            .filter(|e| e.sacked)
            .map(|e| e.seq)
            .collect();

        for entry in self.queue.iter_mut() {
            if entry.sacked || entry.lost {
                continue;
            }
            let segs_above = sacked_seqs.iter()
                .filter(|&&s| seq_after(s, entry.seq))
                .count() as u32;
            if segs_above >= self.reorder_threshold {
                entry.lost = true;
                lost_seqs.push(entry.seq);
            }
        }
        self.sack.lost_count = self.queue.iter().filter(|e| e.lost).count() as u32;

        lost_seqs
    }

    /// Get the next segment to retransmit (marked lost and not yet retransmitted in this round)
    pub fn next_retransmit(&mut self) -> Option<&mut RetransmitEntry> {
        self.queue.iter_mut().find(|e| e.lost && !e.sacked)
    }

    /// Mark a segment as retransmitted
    pub fn mark_retransmitted(&mut self, seq: u32, now_ms: u64) {
        if let Some(entry) = self.queue.iter_mut().find(|e| e.seq == seq) {
            entry.transmit_count += 1;
            entry.tx_time_ms = now_ms;
            entry.lost = false;
            self.retrans_out += 1;
            self.total_retrans += 1;
        }
    }

    /// Handle RTO: mark all segments for retransmission, clear SACK state
    /// Per Linux tcp_enter_loss(): SACK information becomes unreliable after RTO
    pub fn on_rto(&mut self) {
        self.sack.clear();
        for entry in self.queue.iter_mut() {
            entry.sacked = false;
            entry.lost = true; // all are considered lost
        }
        self.retrans_out = 0;
    }

    /// Total bytes in flight (unACKed, including SACKed)
    pub fn bytes_in_flight(&self) -> u32 {
        self.queue.iter()
            .filter(|e| !e.sacked)
            .map(|e| e.end_seq.wrapping_sub(e.seq))
            .sum()
    }

    /// Number of segments in queue
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

// ============================================================================
// Sequence number comparison helpers — handle 32-bit wrap
// ============================================================================

/// Returns true if seq1 is before seq2 (handles wrap-around)
#[inline]
fn seq_before(seq1: u32, seq2: u32) -> bool {
    (seq1 as i32).wrapping_sub(seq2 as i32) < 0
}

/// Returns true if seq1 is after seq2 (handles wrap-around)
#[inline]
fn seq_after(seq1: u32, seq2: u32) -> bool {
    seq_before(seq2, seq1)
}

/// Returns true if seq is in range [lo, hi) with wrap-around handling
#[inline]
fn seq_between(lo: u32, seq: u32, hi: u32) -> bool {
    !seq_before(seq, lo) && seq_before(seq, hi)
}

/// Check if two SACK blocks overlap or are adjacent
#[inline]
fn blocks_overlap_or_adjacent(a: &SackBlock, b: &SackBlock) -> bool {
    // Overlapping or touching: a.left <= b.right && b.left <= a.right
    !seq_after(a.left, b.right) && !seq_after(b.left, a.right)
}
