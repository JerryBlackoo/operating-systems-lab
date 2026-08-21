use core::cmp;

use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
};
use libc_types::{
    poll::PollEvent,
    types::{Stat, StatMode},
};
use sync::Mutex;
use syscalls::Errno;
use vfscore::{INodeInterface, VfsResult};

const PIPE_MIN_CAPACITY: usize = 4096;
pub const PIPE_DEFAULT_CAPACITY: usize = 0x50000;

struct PipeState {
    queue: VecDeque<u8>,
    capacity: usize,
}

pub struct PipeSender(Arc<Mutex<PipeState>>);

impl PipeSender {
    pub fn capacity(&self) -> usize {
        self.0.lock().capacity
    }

    pub fn set_capacity(&self, requested: usize) -> usize {
        let mut state = self.0.lock();
        state.capacity = cmp::max(cmp::max(requested, PIPE_MIN_CAPACITY), state.queue.len());
        state.capacity
    }
}

impl INodeInterface for PipeSender {
    fn writeat(&self, _offset: usize, buffer: &[u8]) -> VfsResult<usize> {
        log::warn!("write pipe:");
        let mut state = self.0.lock();
        if state.queue.len() >= state.capacity {
            return Err(Errno::EWOULDBLOCK);
        }
        let wlen = cmp::min(buffer.len(), state.capacity - state.queue.len());
        state.queue.extend(buffer[..wlen].iter());
        Ok(wlen)
    }

    fn poll(&self, events: PollEvent) -> VfsResult<PollEvent> {
        let mut res = PollEvent::NONE;
        let state = self.0.lock();
        if events.contains(PollEvent::OUT)
            && state.capacity.saturating_sub(state.queue.len()) >= PIPE_MIN_CAPACITY
        {
            res |= PollEvent::OUT;
        }
        Ok(res)
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.mode = StatMode::FIFO;
        Ok(())
    }
}

// pipe reader, just can read.
pub struct PipeReceiver {
    queue: Arc<Mutex<PipeState>>,
    sender: Weak<PipeSender>,
}

impl PipeReceiver {
    pub fn capacity(&self) -> usize {
        self.queue.lock().capacity
    }

    pub fn set_capacity(&self, requested: usize) -> usize {
        let mut state = self.queue.lock();
        state.capacity = cmp::max(cmp::max(requested, PIPE_MIN_CAPACITY), state.queue.len());
        state.capacity
    }
}

impl INodeInterface for PipeReceiver {
    fn readat(&self, _offset: usize, buffer: &mut [u8]) -> VfsResult<usize> {
        let mut state = self.queue.lock();
        let rlen = cmp::min(state.queue.len(), buffer.len());
        state
            .queue
            .drain(..rlen)
            .zip(buffer.iter_mut())
            .for_each(|(src, dst)| *dst = src);
        if rlen == 0 && Weak::strong_count(&self.sender) > 0 {
            Err(Errno::EWOULDBLOCK)
        } else {
            Ok(rlen)
        }
    }

    fn poll(&self, events: PollEvent) -> VfsResult<PollEvent> {
        let mut res = PollEvent::NONE;
        let state = self.queue.lock();
        if events.contains(PollEvent::IN) {
            if !state.queue.is_empty() {
                res |= PollEvent::IN;
            } else if Weak::strong_count(&self.sender) == 0 {
                res |= PollEvent::ERR;
            }
        }
        if events.contains(PollEvent::ERR)
            && state.queue.is_empty()
            && Weak::strong_count(&self.sender) == 0
        {
            res |= PollEvent::ERR;
        }
        Ok(res)
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.mode = StatMode::FIFO;
        Ok(())
    }
}

pub fn create_pipe() -> (Arc<PipeReceiver>, Arc<PipeSender>) {
    let queue = Arc::new(Mutex::new(PipeState {
        queue: VecDeque::new(),
        capacity: PIPE_DEFAULT_CAPACITY,
    }));
    let sender = Arc::new(PipeSender(queue.clone()));
    (
        Arc::new(PipeReceiver {
            queue: queue.clone(),
            sender: Arc::downgrade(&sender),
        }),
        sender,
    )
}
