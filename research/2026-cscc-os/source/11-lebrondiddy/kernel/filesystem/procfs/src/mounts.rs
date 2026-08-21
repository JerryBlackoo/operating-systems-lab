use core::cmp;

use libc_types::types::{Stat, StatMode};
use vfscore::{INodeInterface, VfsResult};

const MOUNTS: &[u8] = b"/dev/vda / ext4 rw,relatime 0 0\n\
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n\
devtmpfs /dev devtmpfs rw,nosuid 0 0\n";

pub struct Mounts {}

impl Mounts {
    pub const fn new() -> Self {
        Self {}
    }
}

impl INodeInterface for Mounts {
    fn readat(&self, offset: usize, buffer: &mut [u8]) -> VfsResult<usize> {
        if offset >= MOUNTS.len() {
            return Ok(0);
        }
        let rsize = cmp::min(MOUNTS.len() - offset, buffer.len());
        buffer[..rsize].copy_from_slice(&MOUNTS[offset..offset + rsize]);
        Ok(rsize)
    }

    fn stat(&self, stat: &mut Stat) -> vfscore::VfsResult<()> {
        stat.dev = 0;
        stat.ino = 1; // TODO: convert path to number(ino)
        stat.mode = StatMode::CHAR; // TODO: add access mode
        stat.nlink = 1;
        stat.uid = 1000;
        stat.gid = 1000;
        stat.size = MOUNTS.len() as _;
        stat.blksize = 512;
        stat.blocks = 0;
        stat.rdev = 0; // TODO: add device id
        Ok(())
    }
}
