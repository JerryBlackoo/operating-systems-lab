use core::cmp;

use libc_types::types::{Stat, StatMode};
use vfscore::{INodeInterface, VfsResult};

const MEMINFO: &[u8] = b"MemTotal:        1048576 kB\n\
MemFree:          524288 kB\n\
MemAvailable:     524288 kB\n\
Buffers:               0 kB\n\
Cached:                0 kB\n\
SwapCached:            0 kB\n\
Active:                0 kB\n\
Inactive:              0 kB\n\
SwapTotal:             0 kB\n\
SwapFree:              0 kB\n";

pub struct MemInfo {}

impl MemInfo {
    pub const fn new() -> Self {
        Self {}
    }
}

impl INodeInterface for MemInfo {
    fn readat(&self, offset: usize, buffer: &mut [u8]) -> VfsResult<usize> {
        if offset >= MEMINFO.len() {
            return Ok(0);
        }
        let rsize = cmp::min(MEMINFO.len() - offset, buffer.len());
        buffer[..rsize].copy_from_slice(&MEMINFO[offset..offset + rsize]);
        Ok(rsize)
    }

    fn stat(&self, stat: &mut Stat) -> vfscore::VfsResult<()> {
        stat.dev = 0;
        stat.ino = 1; // TODO: convert path to number(ino)
        stat.mode = StatMode::FILE;
        stat.nlink = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = MEMINFO.len() as _;
        stat.blksize = 512;
        stat.blocks = 0;
        stat.rdev = 0; // TODO: add device id
        Ok(())
    }
}
