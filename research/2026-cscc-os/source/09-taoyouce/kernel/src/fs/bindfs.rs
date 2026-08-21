//! Bind-mount 文件系统（对标 Linux do_loopback 的子树绑定）
//!
//! Linux 的 bind mount 不创建新文件系统，而是把源路径对应的 dentry 子树
//! 作为新挂载的根（`struct mount.mnt_root = 源 dentry`）。这里用同样语义：
//! BindFs 持有源 inode 作为根，所有读写直接别名到源子树。

use super::*;
use alloc::sync::Arc;

/// Bind 挂载：根 inode 即源路径的 inode（子树别名，非拷贝）。
pub struct BindFs {
    root: Arc<dyn Inode>,
}

impl BindFs {
    pub fn new(root: Arc<dyn Inode>) -> Arc<Self> {
        Arc::new(Self { root })
    }
}

impl FileSystem for BindFs {
    fn name(&self) -> &str {
        "none"
    }

    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}
