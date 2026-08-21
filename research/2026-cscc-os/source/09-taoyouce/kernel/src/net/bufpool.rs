//! Loopback datagram buffer pool.
//!
//! Every loopback send allocates a `Vec<u8>` that travels through the peer's
//! recv queue and is dropped right after the receiver copies it out. On
//! request/response workloads that is one global-heap alloc + dealloc per
//! packet. This pool recycles those buffers: the receive path returns the
//! spent `Vec` here and the send path grabs one back, so the steady state
//! runs allocation-free.

use alloc::vec::Vec;

use crate::sync::SpinLock;

const MAX_POOLED: usize = 32;
const MAX_CAP: usize = 64 * 1024;

static POOL: SpinLock<Vec<Vec<u8>>> = SpinLock::new(Vec::new());

/// Fetch a buffer with `len` initialized (uninit contents — caller must
/// overwrite all `len` bytes before reading them).
pub fn take(len: usize) -> Vec<u8> {
    if len <= MAX_CAP {
        let mut pool = POOL.lock();
        if let Some(mut buf) = pool.pop() {
            drop(pool);
            if buf.capacity() >= len {
                unsafe { buf.set_len(len) };
                return buf;
            }
            // Too small: fall through and let it drop.
        }
    }
    let mut buf = Vec::with_capacity(len);
    unsafe { buf.set_len(len) };
    buf
}

/// Return a spent buffer to the pool.
pub fn put(buf: Vec<u8>) {
    let cap = buf.capacity();
    if cap == 0 || cap > MAX_CAP {
        return;
    }
    let mut pool = POOL.lock();
    if pool.len() < MAX_POOLED {
        pool.push(buf);
    }
}
