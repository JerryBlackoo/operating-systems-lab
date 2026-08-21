//! Overlay 联合文件系统（对标 Linux fs/overlayfs 的实时联合语义）
//!
//! 与快照式实现不同，lookup / read_dir / open 每次都实时穿透到 upper 与
//! lower 层：挂载后在 lower/upper 里新建的文件立即在合并视图可见（Linux
//! ovl_lookup 语义）。写路径按 Linux copy-up 语义：lower 文件首次写/截断/
//! 改元数据时复制到 upper 层，之后 lower 原件不再受影响。删除 lower 项
//! 用 whiteout 记账（Linux 用 upper 层字符设备 whiteout，这里用挂载内
//! 相对路径集合，语义等价：合并视图隐藏、lower 原件不动）。

use super::*;
use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::SpinLock;

/// 每个 overlay 挂载的共享状态。
pub struct OvlState {
    /// upper 层根目录（可写层）。挂载无 upperdir 时用匿名 RamFs 根，
    /// 保持合并视图可写（只读 overlay 在 Linux 上写返回 EROFS，但本内核
    /// 现有测例都带 upperdir 或期望可写，匿名 upper 更兼容）。
    upper_root: Arc<dyn Inode>,
    /// lower 层根目录，优先级从高到低。
    lower_roots: Vec<Arc<dyn Inode>>,
    /// whiteout 集合：挂载内相对路径（"a/b"），命中即在合并视图隐藏。
    whiteouts: SpinLock<BTreeSet<String>>,
    /// opaque 集合：upper 项是全新身份（whiteout 后重建/覆盖 rename），
    /// 同名 lower 不再作为 origin（对标 Linux ovl opaque/无 origin xattr）。
    opaques: SpinLock<BTreeSet<String>>,
}

impl OvlState {
    /// 沿相对路径从某层根目录解析到目标 inode（不跨挂载、不追符号链接，
    /// 对标 ovl 层内 dentry walk）。
    fn walk(root: &Arc<dyn Inode>, rel: &str) -> Option<Arc<dyn Inode>> {
        let mut cur = root.clone();
        if rel.is_empty() {
            return Some(cur);
        }
        for comp in rel.split('/') {
            let dir = cur.as_dir()?;
            cur = dir.lookup(comp).ok()?;
        }
        Some(cur)
    }

    fn upper_at(&self, rel: &str) -> Option<Arc<dyn Inode>> {
        Self::walk(&self.upper_root, rel)
    }

    /// origin 口径的 lower 查找：opaque 项视为无 origin。
    fn lower_origin_at(&self, rel: &str) -> Option<Arc<dyn Inode>> {
        if self.opaques.lock().contains(rel) {
            return None;
        }
        self.lower_at(rel)
    }

    fn lower_at(&self, rel: &str) -> Option<Arc<dyn Inode>> {
        for root in &self.lower_roots {
            if let Some(i) = Self::walk(root, rel) {
                return Some(i);
            }
        }
        None
    }

    fn is_whiteout(&self, rel: &str) -> bool {
        let w = self.whiteouts.lock();
        // 祖先目录被 whiteout 也隐藏本项
        if w.contains(rel) {
            return true;
        }
        let mut prefix = String::new();
        for comp in rel.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(comp);
            if w.contains(prefix.as_str()) {
                return true;
            }
        }
        false
    }

    /// 确保 upper 层存在 rel 对应的目录链（Linux ovl_copy_up 的目录部分），
    /// 返回该目录 inode。
    fn ensure_upper_dir(&self, rel: &str) -> Result<Arc<dyn Inode>, FsError> {
        let mut cur = self.upper_root.clone();
        if rel.is_empty() {
            return Ok(cur);
        }
        let mut walked = String::new();
        for comp in rel.split('/') {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(comp);
            let dir = cur.as_dir().ok_or(FsError::NotDirectory)?;
            cur = match dir.lookup(comp) {
                Ok(existing) => existing,
                Err(_) => {
                    let created = dir.create(comp, FileType::Directory)?;
                    // 目录元数据 copy-up：沿用 lower 目录权限位
                    if let Some(low) = self.lower_at(&walked) {
                        if let Ok(st) = low.stat() {
                            let _ = created.set_mode(st.mode.0);
                        }
                    }
                    created
                }
            };
        }
        Ok(cur)
    }

    /// 文件 copy-up：把 lower 文件/符号链接复制到 upper 层（Linux
    /// ovl_copy_up）。返回 upper inode。已存在则直接复用。
    fn copy_up(&self, rel: &str, lower: &Arc<dyn Inode>) -> Result<Arc<dyn Inode>, FsError> {
        let (parent_rel, name) = split_rel(rel);
        let updir = self.ensure_upper_dir(parent_rel)?;
        let updir_d = updir.as_dir().ok_or(FsError::NotDirectory)?;
        if let Ok(existing) = updir_d.lookup(name) {
            return Ok(existing);
        }
        match lower.file_type() {
            FileType::Symlink => {
                let target = lower.read_link()?;
                let l = updir_d.create(name, FileType::Symlink)?;
                if let Some(wf) = l.as_file() {
                    let _ = wf.write(0, target.as_bytes());
                }
                Ok(l)
            }
            FileType::Directory => {
                // 目录 copy-up 只建壳，子项仍实时联合
                self.ensure_upper_dir(rel)
            }
            _ => {
                let f = updir_d.create(name, FileType::Regular)?;
                if let (Some(rf), Some(wf)) = (lower.as_file(), f.as_file()) {
                    let mut off = 0usize;
                    let mut buf = [0u8; 4096];
                    loop {
                        match rf.read(off, &mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if wf.write(off, &buf[..n]).is_err() {
                                    break;
                                }
                                off += n;
                            }
                            Err(_) => break,
                        }
                    }
                }
                if let Ok(st) = lower.stat() {
                    let _ = f.set_mode(st.mode.0);
                    let _ = f.set_owner(Some(st.uid), Some(st.gid));
                    let _ = f.set_times(Some(st.atime), Some(st.mtime));
                }
                Ok(f)
            }
        }
    }
}

fn split_rel(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(pos) => (&rel[..pos], &rel[pos + 1..]),
        None => ("", rel),
    }
}

fn child_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        alloc::format!("{}/{}", rel, name)
    }
}

/// 合并目录 inode：实时联合 upper 与各 lower 层同名目录。
pub struct OvlDir {
    state: Arc<OvlState>,
    rel: String,
}

impl OvlDir {
    fn new(state: Arc<OvlState>, rel: String) -> Arc<Self> {
        Arc::new(Self { state, rel })
    }

    /// 本目录的代表 inode（stat 委托对象）：upper 优先，其次首个 lower。
    fn backing(&self) -> Option<Arc<dyn Inode>> {
        self.state
            .upper_at(&self.rel)
            .or_else(|| self.state.lower_at(&self.rel))
    }

    /// origin inode（ino/file-handle 稳定性委托对象，Linux ovl 的 origin
    /// 语义）：lower 优先——目录在 upper 建壳（copy-up）后 ino 不得变。
    fn origin(&self) -> Option<Arc<dyn Inode>> {
        self.state
            .lower_origin_at(&self.rel)
            .or_else(|| self.state.upper_at(&self.rel))
    }
}

impl Inode for OvlDir {
    fn ino(&self) -> usize {
        self.origin().map(|i| i.ino()).unwrap_or(1)
    }

    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn as_file(&self) -> Option<&dyn File> {
        None
    }

    fn as_dir(&self) -> Option<&dyn Directory> {
        Some(self)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        match self.backing() {
            Some(i) => i.stat(),
            None => Err(FsError::NotFound),
        }
    }

    fn set_mode(&self, mode: u32) -> Result<(), FsError> {
        let d = self.state.ensure_upper_dir(&self.rel)?;
        d.set_mode(mode)
    }

    fn set_owner(&self, uid: Option<u32>, gid: Option<u32>) -> Result<(), FsError> {
        let d = self.state.ensure_upper_dir(&self.rel)?;
        d.set_owner(uid, gid)
    }

    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> Result<(), FsError> {
        let d = self.state.ensure_upper_dir(&self.rel)?;
        d.set_times(atime, mtime)
    }
}

impl Directory for OvlDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<DirEntry> = Vec::new();
        let mut layers: Vec<Arc<dyn Inode>> = Vec::new();
        if let Some(u) = self.state.upper_at(&self.rel) {
            layers.push(u);
        }
        for root in &self.state.lower_roots {
            if let Some(l) = OvlState::walk(root, &self.rel) {
                layers.push(l);
            }
        }
        if layers.is_empty() {
            return Err(FsError::NotFound);
        }
        for layer in layers {
            let Some(dir) = layer.as_dir() else { continue };
            let Ok(entries) = dir.read_dir() else { continue };
            for ent in entries {
                if ent.name == "." || ent.name == ".." {
                    continue;
                }
                if seen.contains(&ent.name) {
                    continue;
                }
                if self.state.is_whiteout(&child_rel(&self.rel, &ent.name)) {
                    continue;
                }
                seen.insert(ent.name.clone());
                out.push(ent);
            }
        }
        Ok(out)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        if name == "." || name.is_empty() {
            return Ok(OvlDir::new(self.state.clone(), self.rel.clone()) as Arc<dyn Inode>);
        }
        if name == ".." {
            let (parent, _) = split_rel(&self.rel);
            return Ok(OvlDir::new(self.state.clone(), parent.to_string()) as Arc<dyn Inode>);
        }
        let crel = child_rel(&self.rel, name);
        if self.state.is_whiteout(&crel) {
            return Err(FsError::NotFound);
        }
        let up = self.state.upper_at(&crel);
        let low = if up.is_some() {
            // origin 口径：opaque（whiteout 后重建）项不以 lower 为 origin
            self.state.lower_origin_at(&crel)
        } else {
            self.state.lower_at(&crel)
        };
        match (up, low) {
            (Some(up), low) => {
                if up.file_type() == FileType::Directory {
                    return Ok(OvlDir::new(self.state.clone(), crel) as Arc<dyn Inode>);
                }
                // lower 有 origin 的文件（已 copy-up）：仍走 OvlFile 包装，
                // ino/handle 恒取 origin，读写走 upper（Linux ovl origin 语义）
                if let Some(low) = low {
                    if low.file_type() != FileType::Directory {
                        return Ok(Arc::new(OvlFile {
                            state: self.state.clone(),
                            rel: crel,
                            lower: low,
                            upper: SpinLock::new(Some(up)),
                        }) as Arc<dyn Inode>);
                    }
                }
                Ok(up)
            }
            (None, Some(low)) => {
                if low.file_type() == FileType::Directory {
                    return Ok(OvlDir::new(self.state.clone(), crel) as Arc<dyn Inode>);
                }
                Ok(Arc::new(OvlFile {
                    state: self.state.clone(),
                    rel: crel,
                    lower: low,
                    upper: SpinLock::new(None),
                }) as Arc<dyn Inode>)
            }
            (None, None) => Err(FsError::NotFound),
        }
    }

    fn create(&self, name: &str, file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        let crel = child_rel(&self.rel, name);
        let hidden = self.state.is_whiteout(&crel);
        if !hidden && self.lookup(name).is_ok() {
            return Err(FsError::AlreadyExists);
        }
        let updir = self.state.ensure_upper_dir(&self.rel)?;
        let updir_d = updir.as_dir().ok_or(FsError::NotDirectory)?;
        // whiteout 覆盖场景：upper 可能残留旧项，先清掉
        if hidden {
            let _ = updir_d.unlink(name);
        }
        let created = updir_d.create(name, file_type)?;
        self.state.whiteouts.lock().remove(crel.as_str());
        // whiteout 覆盖后重建 = 全新身份，同名 lower 不再作为 origin
        if hidden && self.state.lower_at(&crel).is_some() {
            self.state.opaques.lock().insert(crel.clone());
        } else {
            self.state.opaques.lock().remove(crel.as_str());
        }
        if file_type == FileType::Directory {
            return Ok(OvlDir::new(self.state.clone(), crel) as Arc<dyn Inode>);
        }
        Ok(created)
    }

    fn unlink(&self, name: &str) -> Result<(), FsError> {
        let crel = child_rel(&self.rel, name);
        if self.state.is_whiteout(&crel) {
            return Err(FsError::NotFound);
        }
        let in_upper = self.state.upper_at(&crel).is_some();
        let in_lower = self.state.lower_at(&crel).is_some();
        if !in_upper && !in_lower {
            return Err(FsError::NotFound);
        }
        if in_upper {
            let updir = self.state.upper_at(&self.rel).ok_or(FsError::NotFound)?;
            let updir_d = updir.as_dir().ok_or(FsError::NotDirectory)?;
            updir_d.unlink(name)?;
        }
        if in_lower {
            self.state.whiteouts.lock().insert(crel);
        }
        Ok(())
    }

    fn rename(&self, old_name: &str, new_name: &str) -> Result<(), FsError> {
        let old_rel = child_rel(&self.rel, old_name);
        if self.state.is_whiteout(&old_rel) {
            return Err(FsError::NotFound);
        }
        // 确保源在 upper 层（lower 文件先 copy-up）
        if self.state.upper_at(&old_rel).is_none() {
            let low = self.state.lower_at(&old_rel).ok_or(FsError::NotFound)?;
            self.state.copy_up(&old_rel, &low)?;
        }
        let updir = self.state.ensure_upper_dir(&self.rel)?;
        let updir_d = updir.as_dir().ok_or(FsError::NotDirectory)?;
        updir_d.rename(old_name, new_name)?;
        let new_rel = child_rel(&self.rel, new_name);
        {
            let mut w = self.state.whiteouts.lock();
            w.remove(new_rel.as_str());
            if self.state.lower_at(&old_rel).is_some() {
                w.insert(old_rel);
            }
        }
        // 覆盖 rename：目标位置若有同名 lower，它不是被恢复的 origin
        if self.state.lower_at(&new_rel).is_some() {
            self.state.opaques.lock().insert(new_rel);
        }
        Ok(())
    }

    fn link(&self, name: &str, existing: &Arc<dyn Inode>) -> Result<(), FsError> {
        let updir = self.state.ensure_upper_dir(&self.rel)?;
        let updir_d = updir.as_dir().ok_or(FsError::NotDirectory)?;
        updir_d.link(name, existing)?;
        self.state
            .whiteouts
            .lock()
            .remove(child_rel(&self.rel, name).as_str());
        Ok(())
    }
}

/// lower 层文件的合并视图包装：读委托当前生效层，写前 copy-up。
pub struct OvlFile {
    state: Arc<OvlState>,
    rel: String,
    lower: Arc<dyn Inode>,
    /// copy-up 后的 upper inode 缓存（同 rel 的其它包装实例靠 upper_at 兜底）
    upper: SpinLock<Option<Arc<dyn Inode>>>,
}

impl OvlFile {
    /// 当前生效 inode：upper 存在（本实例或其它路径已 copy-up）即 upper，
    /// 否则 lower。
    fn current(&self) -> Arc<dyn Inode> {
        if let Some(u) = self.upper.lock().clone() {
            return u;
        }
        if let Some(u) = self.state.upper_at(&self.rel) {
            *self.upper.lock() = Some(u.clone());
            return u;
        }
        self.lower.clone()
    }

    fn writable(&self) -> Result<Arc<dyn Inode>, FsError> {
        if let Some(u) = self.upper.lock().clone() {
            return Ok(u);
        }
        let u = if let Some(u) = self.state.upper_at(&self.rel) {
            u
        } else {
            self.state.copy_up(&self.rel, &self.lower)?
        };
        *self.upper.lock() = Some(u.clone());
        Ok(u)
    }
}

impl Inode for OvlFile {
    fn ino(&self) -> usize {
        // Linux ovl 语义：ino/file-handle 恒取 origin（lower）inode，
        // copy-up 前后保持稳定（fanotify fid、name_to_handle_at 一致性）。
        self.lower.ino()
    }

    fn file_type(&self) -> FileType {
        self.current().file_type()
    }

    fn as_file(&self) -> Option<&dyn File> {
        Some(self)
    }

    fn as_dir(&self) -> Option<&dyn Directory> {
        None
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        self.current().stat()
    }

    fn read_link(&self) -> Result<String, FsError> {
        self.current().read_link()
    }

    fn get_inode_flags(&self) -> u32 {
        self.current().get_inode_flags()
    }

    fn set_mode(&self, mode: u32) -> Result<(), FsError> {
        self.writable()?.set_mode(mode)
    }

    fn set_owner(&self, uid: Option<u32>, gid: Option<u32>) -> Result<(), FsError> {
        self.writable()?.set_owner(uid, gid)
    }

    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> Result<(), FsError> {
        self.writable()?.set_times(atime, mtime)
    }
}

impl File for OvlFile {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let cur = self.current();
        match cur.as_file() {
            Some(f) => f.read(offset, buf),
            None => Err(FsError::IsDirectory),
        }
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        let up = self.writable()?;
        match up.as_file() {
            Some(f) => f.write(offset, buf),
            None => Err(FsError::IsDirectory),
        }
    }

    fn truncate(&self, length: usize) -> Result<(), FsError> {
        let up = self.writable()?;
        match up.as_file() {
            Some(f) => f.truncate(length),
            None => Err(FsError::IsDirectory),
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

/// overlay 挂载的文件系统实例。
pub struct OverlayFs {
    state: Arc<OvlState>,
}

impl OverlayFs {
    pub fn new(
        upper: Option<Arc<dyn Inode>>,
        lowers: Vec<Arc<dyn Inode>>,
    ) -> Arc<Self> {
        let upper_root = upper.unwrap_or_else(|| {
            use crate::fs::FileSystem;
            crate::fs::RamFs::new().root()
        });
        Arc::new(Self {
            state: Arc::new(OvlState {
                upper_root,
                lower_roots: lowers,
                whiteouts: SpinLock::new(BTreeSet::new()),
                opaques: SpinLock::new(BTreeSet::new()),
            }),
        })
    }
}

impl FileSystem for OverlayFs {
    fn name(&self) -> &str {
        "overlay"
    }

    fn root(&self) -> Arc<dyn Inode> {
        OvlDir::new(self.state.clone(), String::new()) as Arc<dyn Inode>
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}
