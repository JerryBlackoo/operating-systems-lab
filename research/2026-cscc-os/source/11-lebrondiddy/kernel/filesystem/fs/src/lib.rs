#![no_std]
#![feature(let_chains)]

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;
extern crate bitflags;

pub mod dentry;
#[cfg(root_fs = "fat32")]
mod fatfs_shim;
pub mod file;
pub mod pathbuf;
pub mod pipe;

use alloc::sync::Arc;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use dentry::mount_fs;
use devfs::{DevDir, DevFS};
use devices::get_blk_devices;
#[cfg(root_fs = "ext4")]
use ext4fs::Ext4FileSystem;
use procfs::ProcFS;
use ramfs::RamFs;
use syscalls::Errno;
use vfscore::VfsResult;
pub use vfscore::{FileType, INodeInterface, SeekFrom};

pub fn build_devfs() -> Arc<DevFS> {
    let mut dev_dir = DevDir::new();
    #[cfg(root_fs = "ext4")]
    {
        let block_count = get_blk_devices().len();
        if block_count > 0 {
            dev_dir.add("vda", Arc::new(devfs::Sdx::new(0, mount_ext4, umount_ext4)));
            dev_dir.add(
                "vda1",
                Arc::new(devfs::Sdx::new(0, mount_ext4, umount_ext4)),
            );
            dev_dir.add(
                "vda2",
                Arc::new(devfs::Sdx::new(
                    if block_count > 1 { 1 } else { usize::MAX },
                    mount_test_scratch,
                    umount_test_scratch,
                )),
            );
        }
    }

    DevFS::new_with_dir(dev_dir)
}

#[cfg(root_fs = "ext4")]
fn mount_test_scratch(device_id: usize, path: &str) -> Result<(), Errno> {
    if device_id == usize::MAX {
        mount_fs(RamFs::new(), path);
    } else {
        mount_fs(Ext4FileSystem::new(device_id), path);
    }
    Ok(())
}

#[cfg(root_fs = "ext4")]
fn mount_ext4(device_id: usize, path: &str) -> Result<(), Errno> {
    mount_fs(Ext4FileSystem::new(device_id), path);
    Ok(())
}

#[cfg(root_fs = "ext4")]
fn umount_test_scratch(_device_id: usize, path: &str) -> Result<(), Errno> {
    dentry::umount(path.into())
}

#[cfg(root_fs = "ext4")]
fn umount_ext4(_device_id: usize, path: &str) -> Result<(), Errno> {
    dentry::umount(path.into())
}

pub fn init() {
    info!("fs module initialized");
    // TODO: Identify the filesystem at the device.
    if !get_blk_devices().is_empty() {
        #[cfg(root_fs = "fat32")]
        mount_fs(fatfs_shim::Fat32FileSystem::new(0), "/");
        #[cfg(root_fs = "ext4")]
        mount_fs(ext4fs::Ext4FileSystem::new(0), "/");
        #[cfg(root_fs = "ext4_rs")]
        mount_fs(ext4rsfs::Ext4FileSystem::new(0), "/");
    } else {
        mount_fs(RamFs::new(), "/");
    }
    refresh_runtime_mounts();
    // filesystems.push((RamFs::new(), "/bin"));
}

pub fn refresh_runtime_mounts() {
    mount_fs(build_devfs(), "/dev");
    mount_fs(RamFs::new(), "/bin");
    mount_fs(RamFs::new(), "/tmp");
    mount_fs(RamFs::new(), "/dev/shm");
    mount_fs(RamFs::new(), "/etc");
    mount_fs(RamFs::new(), "/home");
    mount_fs(RamFs::new(), "/var");
    mount_fs(ProcFS::new(), "/proc");
}

pub struct WaitBlockingRead<'a>(pub Arc<dyn INodeInterface>, pub &'a mut [u8], pub usize);

impl Future for WaitBlockingRead<'_> {
    type Output = VfsResult<usize>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let offset = self.2;
        let file = self.0.clone();
        let buffer = &mut self.1;
        match file.readat(offset, buffer) {
            Ok(rsize) => Poll::Ready(Ok(rsize)),
            Err(err) => {
                if let Errno::EWOULDBLOCK = err {
                    Poll::Pending
                } else {
                    Poll::Ready(Err(err))
                }
            }
        }
    }
}

pub struct WaitBlockingWrite<'a>(pub Arc<dyn INodeInterface>, pub &'a [u8], pub usize);

impl Future for WaitBlockingWrite<'_> {
    type Output = VfsResult<usize>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let offset = self.2;
        let file = self.0.clone();
        let buffer = &self.1;

        match file.writeat(offset, buffer) {
            Ok(wsize) => Poll::Ready(Ok(wsize)),
            Err(err) => {
                if let Errno::EWOULDBLOCK = err {
                    Poll::Pending
                } else {
                    Poll::Ready(Err(err))
                }
            }
        }
    }
}
