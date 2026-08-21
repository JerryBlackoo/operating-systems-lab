use hashbrown::HashMap;
use libc_types::{
    epoll::{EpollCtl, EpollEvent},
    poll::PollEvent,
};
use sync::Mutex;
use vfscore::INodeInterface;

#[derive(Debug)]
pub struct EpollInterest {
    pub event: EpollEvent,
    pub last_ready: PollEvent,
    pub disabled: bool,
}

#[derive(Debug)]
pub struct EpollFile {
    pub data: Mutex<HashMap<usize, EpollInterest>>,
    pub flags: usize,
}

impl EpollFile {
    pub fn new(flags: usize) -> Self {
        EpollFile {
            data: Mutex::new(HashMap::new()),
            flags,
        }
    }

    pub fn ctl(&self, ctl: EpollCtl, fd: usize, ev: EpollEvent) {
        let mut data_map = self.data.lock();
        match ctl {
            EpollCtl::ADD => {
                data_map.insert(
                    fd,
                    EpollInterest {
                        event: ev,
                        last_ready: PollEvent::NONE,
                        disabled: false,
                    },
                );
            }
            EpollCtl::DEL => {
                data_map.remove(&fd);
            }
            EpollCtl::MOD => {
                data_map.get_mut(&fd).map(|x| {
                    x.event = ev;
                    x.last_ready = PollEvent::NONE;
                    x.disabled = false;
                });
            }
        }
    }
}

impl INodeInterface for EpollFile {}
