use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs;
use crate::syscall::SyscallError;
use crate::sync::SpinLock;

#[derive(Clone)]
struct PrefixCacheEntry {
    ino: usize,
    size: usize,
    mtime: u64,
    ctime: u64,
    bytes: Vec<u8>,
}

static EXEC_PREFIX_CACHE: SpinLock<Option<BTreeMap<(String, usize), PrefixCacheEntry>>> = SpinLock::new(None);
const EXEC_PREFIX_CACHE_MAX_ENTRIES: usize = 64;

fn cache_get(path: &str, max_len: usize, ino: usize, size: usize, mtime: u64, ctime: u64) -> Option<Vec<u8>> {
    let guard = EXEC_PREFIX_CACHE.lock();
    let entry = guard.as_ref()?.get(&(path.to_string(), max_len))?;
    if entry.ino == ino && entry.size == size && entry.mtime == mtime && entry.ctime == ctime {
        Some(entry.bytes.clone())
    } else {
        None
    }
}

fn cache_put(path: &str, max_len: usize, ino: usize, size: usize, mtime: u64, ctime: u64, bytes: &[u8]) {
    let mut guard = EXEC_PREFIX_CACHE.lock();
    let map = guard.get_or_insert_with(BTreeMap::new);
    if !map.contains_key(&(path.to_string(), max_len)) && map.len() >= EXEC_PREFIX_CACHE_MAX_ENTRIES {
        if let Some(oldest_key) = map.keys().next().cloned() {
            map.remove(&oldest_key);
        }
    }
    map.insert(
        (path.to_string(), max_len),
        PrefixCacheEntry {
            ino,
            size,
            mtime,
            ctime,
            bytes: bytes.to_vec(),
        },
    );
}

pub(super) fn read_file_prefix_from_fs(path: &str, max_len: usize) -> Result<Vec<u8>, SyscallError> {
    let inode = fs::lookup(path).map_err(|_| SyscallError::NotFound)?;
    let stat = inode.stat().map_err(|_| SyscallError::InvalidArgument)?;
    let ino = inode.ino();
    let immutable = inode.is_immutable();

    if immutable {
        if let Some(bytes) = cache_get(path, max_len, ino, stat.size, stat.mtime, stat.ctime) {
            return Ok(bytes);
        }
    }

    let read_len = core::cmp::min(stat.size, max_len);
    if read_len == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let file = inode.as_file().ok_or(SyscallError::InvalidArgument)?;
    let mut buf = alloc::vec![0u8; read_len];
    let bytes_read = file.read(0, &mut buf).map_err(|_| SyscallError::InvalidArgument)?;
    buf.truncate(bytes_read);
    if buf.is_empty() {
        return Err(SyscallError::InvalidArgument);
    }

    if immutable {
        cache_put(path, max_len, ino, stat.size, stat.mtime, stat.ctime, &buf);
    }

    Ok(buf)
}

// ── execve 全镜像缓存 ────────────────────────────────────────────────────
//
// `load_elf_from_fs()` 每次 execve 都 `vec![0u8; stat.size]` 再一次读完整个
// ELF。BuildStorm 主编译会把 `cargo`(16.5MB) / `rustc`(5.3MB) / busybox 工具
// 启动**数百次**，于是产生数 GB 的重复读 + 重复大块分配 + 重复拷贝。实测
// `cargo new`（只建一个目录和两个文件）耗时 28.6 秒、一次 fork+exec+read
// 平均 71.7ms，正是被这条路径拖住。
//
// Linux 的 `execve` 从不整文件读入：只读前 128 字节判格式，再按 `PT_LOAD`
// 建立**文件映射**做 demand paging。改成 demand paging 是后续架构工作；这里
// 先做等价且零语义风险的一步：按 `(ino, size, mtime, ctime)` 四元组缓存已读
// 过的镜像。四元组任一变化即失效重读，与页缓存的一致性判据相同。
//
// 容量按**总字节数**限制（不是条数），避免几个大二进制把内核堆吃穿。

struct ImageCacheEntry {
    ino: usize,
    size: usize,
    mtime: u64,
    ctime: u64,
    bytes: alloc::sync::Arc<Vec<u8>>,
}

static EXEC_IMAGE_CACHE: SpinLock<Option<BTreeMap<String, ImageCacheEntry>>> =
    SpinLock::new(None);

/// 缓存总量上限。cargo(16.5MB) + rustc(5.3MB) + 常用 busybox 工具约数十 MB，
/// 相对 8GB guest RAM 可忽略，换掉的是数百次全量重读。
const EXEC_IMAGE_CACHE_MAX_BYTES: usize = 192 * 1024 * 1024;
/// 单个镜像上限：超过这个大小的可执行文件不缓存（避免单次吃掉整个预算）。
const EXEC_IMAGE_MAX_ONE: usize = 64 * 1024 * 1024;

fn image_cache_total(map: &BTreeMap<String, ImageCacheEntry>) -> usize {
    map.values().map(|e| e.bytes.len()).sum()
}

/// 读取（并缓存）一个可执行镜像的全部内容。
///
/// 返回 `Arc<Vec<u8>>`：命中缓存时只是一次引用计数增加，不再拷贝 16MB。
pub(super) fn read_exec_image(path: &str) -> Result<alloc::sync::Arc<Vec<u8>>, SyscallError> {
    let inode = fs::lookup(path).map_err(|_| SyscallError::NotFound)?;
    let stat = inode.stat().map_err(|_| SyscallError::InvalidArgument)?;
    let ino = inode.ino();
    let size = stat.size;
    if size == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    // 命中判据与页缓存一致：ino + size + mtime + ctime 全部相同。
    {
        let guard = EXEC_IMAGE_CACHE.lock();
        if let Some(map) = guard.as_ref() {
            if let Some(e) = map.get(path) {
                if e.ino == ino && e.size == size && e.mtime == stat.mtime && e.ctime == stat.ctime
                {
                    return Ok(e.bytes.clone());
                }
            }
        }
    }

    let file = inode.as_file().ok_or(SyscallError::InvalidArgument)?;
    let mut buf = alloc::vec![0u8; size];
    let read_len = file
        .read(0, &mut buf)
        .map_err(|_| SyscallError::InvalidArgument)?;
    if read_len != size {
        return Err(SyscallError::InvalidArgument);
    }
    let arc = alloc::sync::Arc::new(buf);

    if size <= EXEC_IMAGE_MAX_ONE {
        let mut guard = EXEC_IMAGE_CACHE.lock();
        let map = guard.get_or_insert_with(BTreeMap::new);
        // 预算不足时按「最大的先丢」腾空间：大镜像占用最多，且通常是
        // 一次性工具；反复启动的 cargo/rustc 会立刻被重新填回。
        while image_cache_total(map) + size > EXEC_IMAGE_CACHE_MAX_BYTES && !map.is_empty() {
            if let Some(victim) = map
                .iter()
                .max_by_key(|(_, e)| e.bytes.len())
                .map(|(k, _)| k.clone())
            {
                map.remove(&victim);
            } else {
                break;
            }
        }
        if image_cache_total(map) + size <= EXEC_IMAGE_CACHE_MAX_BYTES {
            map.insert(
                path.to_string(),
                ImageCacheEntry {
                    ino,
                    size,
                    mtime: stat.mtime,
                    ctime: stat.ctime,
                    bytes: arc.clone(),
                },
            );
        }
    }

    Ok(arc)
}
