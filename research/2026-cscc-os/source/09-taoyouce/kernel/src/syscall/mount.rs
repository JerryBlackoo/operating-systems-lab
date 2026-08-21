//! Mount subsystem — per Linux fs/namespace.c, fs/fsopen.c, fs/open.c
//!
//! Extracted from compat.rs. All mount-related syscalls live here:
//! * mount(2) — traditional mount with MS_* flags
//! * umount2(2) — unmount with MNT_FORCE/MNT_DETACH/MNT_EXPIRE
//! * mount_setattr(2) — Linux 5.12+ mount attribute changes
//! * fsopen(2) / fsconfig(2) / fsmount(2) — new mount API (Linux 5.2+)
//! * fspick(2) — pick existing mount for reconfiguration
//! * open_tree(2) — clone mount subtree
//! * move_mount(2) — move mount between locations
//! * statmount(2) / listmount(2) — mount information queries (Linux 6.8+)

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use super::user::read_c_string;

// ============================================================================
// Helper: current effective UID
// ============================================================================

fn current_euid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.euid))
        .unwrap_or(0)
}

fn is_known_fstype(name: &str) -> bool {
    matches!(
        name,
        "tmpfs" | "proc" | "sysfs" | "devtmpfs" | "devpts" |
        "ext2" | "ext3" | "ext4" |
        "vfat" | "msdos" | "fat32" | "fat" |
        "ramfs" | "rootfs" | "fuse" | "none" |
        "cgroup" | "cgroup2" | "overlay" | "squashfs" |
        "hugetlbfs"
    )
}

fn is_block_fs(name: &str) -> bool {
    matches!(name, "ext2" | "ext3" | "ext4" | "vfat" | "msdos" | "fat32" | "fat" | "squashfs")
}

// ============================================================================
// Persistent device-backed filesystem registry
// ============================================================================
//
// The kernel fakes block-device filesystems with RamFs. A plain umount pops the
// RamFs off the mount stack, discarding its content. That is correct across a
// reformat (mkfs writes a fresh filesystem), but it breaks the common LTP idiom
// of umount + remount of the *same* device *without* mkfs (e.g. fanotify10's
// mount_cycle()): the files created before the cycle must survive.
//
// We therefore keep a persistent RamFs per (device, fstype). A block-device
// mount reuses the registered fs so content survives umount/remount; a real
// reformat (mkfs.*) drops the entry via device_fs_reset() so the next mount
// starts empty — matching Linux semantics. Keying by fstype as well as device
// preserves the older fix where cycling fstypes (ext2 -> vfat -> ...) on one
// device must each present a fresh filesystem.
static DEVICE_FS: crate::sync::SpinLock<alloc::collections::BTreeMap<String, alloc::sync::Arc<dyn crate::fs::FileSystem>>> =
    crate::sync::SpinLock::new(alloc::collections::BTreeMap::new());

fn device_fs_key(device: &str, fstype: &str) -> String {
    alloc::format!("{}\u{0}{}", device, fstype)
}

// ============================================================================
// overlayfs — minimal union mount (per Linux fs/overlayfs)
// ============================================================================
//
// LTP's inotify07/inotify08 mount an overlay with
// `lowerdir=...,upperdir=...,workdir=...` and then watch paths *inside* the
// merged mount. Both tests are path-based (inotify), so a faithful union view
// at mount time is sufficient: we materialise the merged tree into a fresh
// RamFs by copying the lower layers (lowest priority last) and then the upper
// layer on top (upper wins). Subsequent writes/chmod/touch on the merged mount
// hit the RamFs directly, and — crucially for inotify08 — they are independent
// of the original lower/upper inodes, so events on lower/upper do not leak into
// a watch on the overlay (and vice-versa), matching real overlayfs copy-up.
fn overlay_opt<'a>(data: &'a str, key: &str) -> Option<&'a str> {
    data.split(',')
        .find_map(|o| o.strip_prefix(key))
}

/// Build a live-union overlay mount from its `lowerdir`/`upperdir` options
/// (per Linux fs/overlayfs). Lower layers are colon-separated, highest
/// priority first; the upper layer (if any) is the writable layer and wins
/// over all lowers. Lookups go through to the layers at access time, so
/// files created in lower/upper after the mount are visible in the merged
/// view; writes to lower files copy-up into the upper layer.
fn build_overlay_fs(data: &str) -> alloc::sync::Arc<dyn crate::fs::FileSystem> {
    let mut lowers: Vec<alloc::sync::Arc<dyn crate::fs::Inode>> = Vec::new();
    if let Some(lower) = overlay_opt(data, "lowerdir=") {
        for layer in lower.split(':') {
            if layer.is_empty() {
                continue;
            }
            let abs = super::open::resolve_path(layer);
            if let Ok(i) = crate::fs::lookup(&abs) {
                lowers.push(i);
            }
        }
    }
    let upper = overlay_opt(data, "upperdir=")
        .filter(|u| !u.is_empty())
        .and_then(|u| crate::fs::lookup(&super::open::resolve_path(u)).ok());
    crate::fs::overlayfs::OverlayFs::new(upper, lowers)
}

/// Get (or lazily create) the persistent RamFs backing `device` mounted as
/// `fstype`. Reused across umount/remount so file content survives a cycle.
fn device_fs_get_or_create(device: &str, fstype: &str) -> alloc::sync::Arc<dyn crate::fs::FileSystem> {
    let key = device_fs_key(device, fstype);
    let mut map = DEVICE_FS.lock();
    if let Some(fs) = map.get(&key) {
        return fs.clone();
    }
    let fs: alloc::sync::Arc<dyn crate::fs::FileSystem> = crate::fs::RamFs::new();
    map.insert(key, fs.clone());
    fs
}

/// Drop every persistent fs registered for `device` (all fstypes). Called when a
/// device is reformatted (mkfs.*) so the next mount sees an empty filesystem.
pub(crate) fn device_fs_reset(device: &str) {
    let prefix = alloc::format!("{}\u{0}", device);
    let mut map = DEVICE_FS.lock();
    let keys: Vec<String> = map.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    for k in keys {
        map.remove(&k);
    }
}

// ============================================================================
// New mount API context (fsopen/fsconfig/fsmount/fspick)
// ============================================================================

struct FsContext {
    fs_name: String,
    source: Option<String>,
    options: Vec<(String, String)>,
    mounted: bool,
    /// Cumulative length of the legacy option buffer ("key=value,key2,...").
    /// Linux caps this at one page in legacy_parse_param() (CVE-2022-0185 fix);
    /// once exceeded fsconfig() must return EINVAL instead of growing forever.
    legacy_size: usize,
}

static FS_CONTEXTS: crate::sync::SpinLock<alloc::collections::BTreeMap<i32, FsContext>> =
    crate::sync::SpinLock::new(alloc::collections::BTreeMap::new());

/// A detached mount produced by fsmount(2): the filesystem has been fully
/// configured (fsopen → fsconfig → CMD_CREATE) but is not yet attached to the
/// directory tree. move_mount(2) with the returned mount fd (MOVE_MOUNT_F_EMPTY_PATH)
/// is what actually grafts it onto a mount point (对标 Linux: fsmount creates a
/// `struct mount` held by an anon `O_PATH`-like fd, move_mount attaches it).
struct PendingMount {
    fs_name: String,
    source: String,
    data: String,
    attr_flags: u32,
}

static PENDING_MOUNTS: crate::sync::SpinLock<alloc::collections::BTreeMap<i32, PendingMount>> =
    crate::sync::SpinLock::new(alloc::collections::BTreeMap::new());

/// True if `fd` is a currently-open file descriptor in the calling process.
fn fd_is_valid(fd: i32) -> bool {
    if fd < 0 { return false; }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref().and_then(|t| t.current.and_then(|pid| {
        t.get(pid).map(|p| (fd as usize) < p.fd_table.len() && p.fd_table[fd as usize].is_some())
    })).unwrap_or(false)
}

/// Instantiate the in-kernel filesystem object for a mount, dispatching by
/// fstype exactly like the normal sys_mount path (per Linux get_fs_type).
fn create_fs_for_mount(
    fstype: &str,
    source: &str,
    data: &str,
    target_abs: &str,
) -> alloc::sync::Arc<dyn crate::fs::FileSystem> {
    match fstype {
        "sysfs" => crate::fs::sysfs::SysFs::new(),
        "cgroup2" => crate::fs::cgroupfs::CgroupFs::new(),
        "cgroup" => {
            if data.split(',').any(|o| o == "memory") || target_abs.ends_with("/memory") {
                crate::fs::memcgfs::MemcgFs::new()
            } else {
                crate::fs::cgroupfs::CgroupFs::new()
            }
        }
        _ if is_block_fs(fstype) && source != "none" && !source.is_empty() => {
            device_fs_get_or_create(source, fstype)
        }
        "overlay" => build_overlay_fs(data),
        _ => crate::fs::RamFs::new(),
    }
}

fn alloc_anon_fd(label: &str) -> i32 {
    alloc_anon_fd_flags(label, 0)
}

/// Like `alloc_anon_fd` but stamps the anon fd with `flags` (e.g. O_PATH for
/// open_tree(2), whose fd carries FMODE_PATH on Linux — it has no associated
/// struct file usable for I/O, so syscalls like accept() fail with EBADF).
fn alloc_anon_fd_flags(label: &str, flags: u32) -> i32 {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let pid = match table.as_ref().and_then(|t| t.current) {
        Some(p) => p,
        None => return -1,
    };
    let proc = match table.as_mut().and_then(|t| t.get_fd_process_mut(pid)) {
        Some(p) => p,
        None => return -1,
    };
    let mut dummy = crate::fs::FileDescriptor::dummy();
    dummy.path = Some(String::from(label));
    dummy.flags = flags;
    match crate::fs::fd::alloc(proc, alloc::sync::Arc::new(dummy), 0, true) {
        Ok(fd) => fd as i32,
        Err(_) => -24,
    }
}

// ============================================================================
// mount(2) — per Linux fs/namespace.c do_mount()
// ============================================================================

pub(in crate::syscall) fn sys_mount(
    source_ptr: *const u8,
    target: *const u8,
    fstype_ptr: *const u8,
    flags: usize,
    data_ptr: *const u8,
) -> isize {
    const MS_RDONLY: usize     = 1;
    const MS_NOSUID: usize     = 2;
    const MS_NODEV: usize      = 4;
    const MS_NOEXEC: usize     = 8;
    const MS_REMOUNT: usize    = 32;
    const MS_NOSYMFOLLOW: usize = 256;
    const MS_NOATIME: usize    = 1024;
    const MS_NODIRATIME: usize = 2048;
    const MS_BIND: usize       = 4096;
    const MS_MOVE: usize       = 8192;
    const MS_STRICTATIME: usize = 1 << 24;
    const MS_UNBINDABLE: usize = 1 << 17;
    const MS_PRIVATE: usize    = 1 << 18;
    const MS_SLAVE: usize      = 1 << 19;
    const MS_SHARED: usize     = 1 << 20;
    const _MS_REC: usize        = 0x4000;

    let euid = current_euid();
    if euid != 0 { return -1; }
    if target.is_null() { return -14; }
    let target = match super::user::read_path(target) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if target.is_empty() { return -2; }

    let source = if source_ptr.is_null() {
        String::from("none")
    } else {
        match super::user::read_path(source_ptr) {
            Ok(s) => if s.is_empty() { String::from("none") } else { s },
            Err(e) => return e,
        }
    };

    let fstype = if fstype_ptr.is_null() {
        String::new()
    } else {
        match read_c_string(fstype_ptr) {
            Ok(s) => s,
            Err(_) => return -14,
        }
    };

    // Mount options (5th arg). For `cgroup` (v1) this names the attached
    // controller(s), e.g. "memory" → the v1 memory controller hierarchy.
    let data = if data_ptr.is_null() {
        String::new()
    } else {
        read_c_string(data_ptr).unwrap_or_default()
    };

    let target_abs = super::open::resolve_path(&target);

    // Mount propagation flags (对标 Linux do_change_type)
    let propagation_mask = MS_SHARED | MS_PRIVATE | MS_SLAVE | MS_UNBINDABLE;
    // 某些 mount(8)/busybox 把 `--bind --make-{slave,...}` 合成单条 syscall
    // (MS_BIND 与传播位同时置位)。此时必须先做 bind 再对结果挂载应用传播变更,
    // 不能在此分支提前 return 把 bind 吞掉。纯传播变更 (无 MS_BIND) 才走这里。
    if flags & propagation_mask != 0 && flags & MS_BIND == 0 {
        if !crate::fs::is_mount_point(&target_abs) {
            return -22; // EINVAL: target must be an existing mount point
        }
        let recursive = flags & _MS_REC != 0;
        let prop = if flags & MS_SHARED != 0 {
            crate::fs::mount::MountPropagation::Shared
        } else if flags & MS_SLAVE != 0 {
            crate::fs::mount::MountPropagation::Slave
        } else if flags & MS_UNBINDABLE != 0 {
            crate::fs::mount::MountPropagation::Unbindable
        } else {
            crate::fs::mount::MountPropagation::Private
        };
        crate::fs::mount::set_mount_propagation(&target_abs, prop, recursive);
        return 0;
    }

    // MS_REMOUNT
    if flags & MS_REMOUNT != 0 {
        if !crate::fs::is_mount_point(&target_abs) {
            return -22;
        }
        if flags & MS_RDONLY != 0 && crate::fs::has_open_writers_under(&target_abs) {
            return -16;
        }
        crate::fs::set_readonly_mount(&target_abs, flags & MS_RDONLY != 0);
        crate::fs::set_noexec_mount(&target_abs, flags & MS_NOEXEC != 0);
        crate::fs::set_nosuid_mount(&target_abs, flags & MS_NOSUID != 0);
        crate::fs::set_nodev_mount(&target_abs, flags & MS_NODEV != 0);
        crate::fs::set_noatime_mount(&target_abs, flags & MS_NOATIME != 0);
        crate::fs::set_nodiratime_mount(&target_abs, flags & MS_NODIRATIME != 0);
        crate::fs::set_nosymfollow_mount(&target_abs, flags & MS_NOSYMFOLLOW != 0);
        {
            let cur_ns = crate::fs::mount::current_mnt_ns();
            let mut table = crate::fs::MOUNT_INFO_TABLE.lock();
            let mut found = false;
            for m in table.iter_mut() {
                if m.target == target_abs && m.ns_id == cur_ns {
                    m.flags &= !(MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_NOSYMFOLLOW | MS_NOATIME | MS_NODIRATIME | MS_STRICTATIME);
                    m.flags |= flags & (MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_NOSYMFOLLOW | MS_NOATIME | MS_NODIRATIME | MS_STRICTATIME);
                    found = true;
                    break;
                }
            }
            if !found {
                drop(table);
                crate::fs::mount_info_add("none", &target_abs, "tmpfs", flags & !(MS_REMOUNT));
            }
        }
        return 0;
    }

    // MS_BIND (对标 Linux do_loopback)
    if flags & MS_BIND != 0 {
        let source_abs = super::open::resolve_path(&source);
        // Unbindable mounts cannot be bind-mounted (对标 Linux IS_MNT_UNBINDABLE)
        {
            let cur_ns = crate::fs::mount::current_mnt_ns();
            let mt = crate::fs::MOUNT_INFO_TABLE.lock();
            if let Some(m) = mt.iter().find(|m| m.target == source_abs && m.ns_id == cur_ns) {
                if m.propagation == crate::fs::MountPropagation::Unbindable {
                    return -22; // EINVAL
                }
            }
        }
        match crate::fs::lookup(&source_abs) {
            Ok(src_inode) => {
                match crate::fs::lookup(&target_abs) {
                    Ok(tgt_inode) => {
                        if tgt_inode.file_type() != crate::fs::FileType::Directory {
                            return -20;
                        }
                    }
                    Err(_) => {
                        return -2;
                    }
                }
                // 对标 Linux do_loopback：bind 挂载的根 = 源路径的 dentry 子树
                // 别名（源是挂载点时 lookup 已解析到栈顶挂载的根）。
                let bind_fs = crate::fs::bindfs::BindFs::new(src_inode);
                crate::fs::mount(&target_abs, bind_fs.clone());
                crate::fs::set_readonly_mount(&target_abs, flags & MS_RDONLY != 0);
                crate::fs::set_noexec_mount(&target_abs, flags & MS_NOEXEC != 0);
                crate::fs::set_nosuid_mount(&target_abs, flags & MS_NOSUID != 0);
                crate::fs::set_nodev_mount(&target_abs, flags & MS_NODEV != 0);
                crate::fs::set_noatime_mount(&target_abs, flags & MS_NOATIME != 0);
                crate::fs::set_nodiratime_mount(&target_abs, flags & MS_NODIRATIME != 0);
                crate::fs::set_nosymfollow_mount(&target_abs, flags & MS_NOSYMFOLLOW != 0);
                let propagated = crate::fs::mount_info_add(&source_abs, &target_abs, "none", flags);
                // shared 传播必须创建真实挂载（对标 propagate_mnt），不只记
                // mountinfo 条目。每个副本创建在其接收方的命名空间里。
                for (pt, ns) in &propagated {
                    crate::fs::mount_in_ns(*ns, pt, bind_fs.clone());
                }
                // 源挂载为 shared 时新挂载与源互为 peer（对标 do_loopback）。
                let cur_ns = crate::fs::mount::current_mnt_ns();
                let mut created: Vec<(String, usize)> = alloc::vec![(target_abs.clone(), cur_ns)];
                created.extend(propagated.iter().cloned());
                crate::fs::mount::bind_join_source_peer_group(&source_abs, &created);
                // MS_REC|MS_BIND: recursively bind all sub-mounts (对标 Linux copy_tree)
                if flags & _MS_REC != 0 {
                    // Snapshot each source submount entry *with its own*
                    // propagation state, in table (= stack) order. A path may
                    // carry several stacked entries (e.g. a shared base + a
                    // slave mount on top); each clone must inherit the
                    // propagation of its *specific* source entry, matched by
                    // stack position — not the topmost entry at the source
                    // path (对标 Linux copy_tree+clone_mnt 逐条对应).
                    let submounts: Vec<(u32, u32, String, String, String, usize,
                                        crate::fs::mount::MountPropagation, u32, u32, String)> = {
                        let mt = crate::fs::MOUNT_INFO_TABLE.lock();
                        mt.iter()
                            .filter(|m| m.ns_id == cur_ns && m.target.starts_with(&alloc::format!("{}/", source_abs)) && m.target != source_abs)
                            .map(|m| (m.mount_id, m.parent_id, m.source.clone(), m.target.clone(), m.fstype.clone(),
                                      m.flags, m.propagation, m.peer_group, m.master_group, m.root_path.clone()))
                            .collect()
                    };
                    // copy_tree 跳过 unbindable 挂载及其下方/栈顶的所有子挂载
                    // (对标 Linux: IS_MNT_UNBINDABLE 时整棵子树不复制)。按表
                    // (栈)顺序传递 skip 集：parent_id 落在 skip 集里的也跳过，
                    // 这样堆叠在 unbindable 之上的挂载 (rel='') 一并被剔除。
                    let mut skip_ids: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
                    for (mid, pid, _, _, _, _, pp, _, _, _) in &submounts {
                        if *pp == crate::fs::mount::MountPropagation::Unbindable
                            || skip_ids.contains(pid) {
                            skip_ids.push(*mid);
                        }
                    }
                    let mut vfs_done: Vec<String> = Vec::new();
                    for (sub_mid, _sub_pid, sub_src, sub_target, sub_fstype, sub_flags,
                         sub_pp, sub_pg, sub_master, sub_root_path) in submounts {
                        if skip_ids.contains(&sub_mid) { continue; }
                        let rel = sub_target.strip_prefix(source_abs.as_str()).unwrap_or("");
                        let new_target = alloc::format!("{}{}", target_abs, rel);
                        // copy_tree：把源路径下的完整挂载栈整段复制到目标路径，
                        // 每层保留各自的根。逐层都 bind 到 lookup(栈顶) 会让下层
                        // (如 self-bind) 错误地显示栈顶挂载 (如 DISK3) 的内容。
                        // 同一路径只复制一次 VFS 栈，但表/传播按条登记。
                        if !vfs_done.contains(&new_target) {
                            crate::fs::clone_mount_stack(&sub_target, &new_target);
                            vfs_done.push(new_target.clone());
                        }
                        let _ = crate::fs::mount_info_add_ex(&sub_src, &new_target, &sub_fstype, sub_flags, false);
                        crate::fs::mount::set_topmost_root_path(&new_target, &sub_root_path);
                        // Decide the cloned submount's propagation. When the
                        // destination parent is shared (propagated 非空, 顶层已
                        // 向其 peers 复制了根副本), 整棵被挂上去的子树都要变成
                        // shared (对标 Linux attach_recursive_mnt: IS_MNT_SHARED
                        // (dest) => 对新树每个 mount set_mnt_shared)。这样即便
                        // 源子挂载是 private (如本例 parent1/child1), 其克隆
                        // parent2/child1 仍是 shared, 之后挂在它下面的 DISK3 才
                        // 会传播到对端 share2/child1。克隆与各对端副本同属一个
                        // 新 peer group; 源本就是 shared 时沿用源的 peer group
                        // (与源互为 peer), 源是 slave 时保留 master (slave+shared)。
                        let (clone_pp, clone_pg, clone_master) = if !propagated.is_empty() {
                            let g = if sub_pg != 0 { sub_pg } else { crate::fs::mount::alloc_peer_group_pub() };
                            (crate::fs::mount::MountPropagation::Shared, g, sub_master)
                        } else {
                            (sub_pp, sub_pg, sub_master)
                        };
                        crate::fs::mount::set_topmost_propagation(
                            &new_target, clone_pp, clone_pg, clone_master);
                        // 把整棵树挂到 shared 目标父时, 子挂载也必须随整棵树
                        // 传播到目标父的每个 peer (对标 Linux attach_recursive_mnt
                        // → propagate_mnt: 对端收到的是源树的完整副本, 不止根)。
                        // `propagated` 是顶层 mount_info_add 为目标父的 peers 创建
                        // 的根副本路径 (如 share2); 每条子挂载在每个根副本下复制
                        // 一份内容 (clone_mount_stack) 并登记表条目, 否则对端只有
                        // 根 (share2) 而缺子挂载 (share2/child1)。
                        // `propagated` carries (root_copy_path, receiver_ns) for
                        // every peer of the destination parent. In the cases
                        // exercised here the peers live in the current namespace
                        // (single-ns rbind trees); cross-ns peers only arise when
                        // rbinding a bare disk image, which has no submounts, so
                        // this loop body is not reached for those.
                        for (peer_root, _peer_ns) in &propagated {
                            let peer_target = alloc::format!("{}{}", peer_root, rel);
                            if !vfs_done.contains(&peer_target) {
                                crate::fs::clone_mount_stack(&sub_target, &peer_target);
                                vfs_done.push(peer_target.clone());
                            }
                            let _ = crate::fs::mount_info_add_ex(&sub_src, &peer_target, &sub_fstype, sub_flags, false);
                            crate::fs::mount::set_topmost_root_path(&peer_target, &sub_root_path);
                            crate::fs::mount::set_topmost_propagation(
                                &peer_target, clone_pp, clone_pg, clone_master);
                        }
                    }
                }
                // 合并的 `--bind --make-{slave,shared,...}`: bind 完成后对刚创建
                // 的栈顶挂载应用传播变更 (对标 util-linux 先 bind 后 do_change_type)。
                if flags & propagation_mask != 0 {
                    let recursive = flags & _MS_REC != 0;
                    let prop = if flags & MS_SHARED != 0 {
                        crate::fs::mount::MountPropagation::Shared
                    } else if flags & MS_SLAVE != 0 {
                        crate::fs::mount::MountPropagation::Slave
                    } else if flags & MS_UNBINDABLE != 0 {
                        crate::fs::mount::MountPropagation::Unbindable
                    } else {
                        crate::fs::mount::MountPropagation::Private
                    };
                    crate::fs::mount::set_mount_propagation(&target_abs, prop, recursive);
                }
                return 0;
            }
            Err(_) => {
                return -2;
            }
        }
    }

    // MS_MOVE
    if flags & MS_MOVE != 0 {
        let source_abs = super::open::resolve_path(&source);
        if !crate::fs::is_mount_point(&source_abs) {
            return -22;
        }
        match crate::fs::lookup(&target_abs) {
            Ok(tgt_inode) => {
                if tgt_inode.file_type() != crate::fs::FileType::Directory {
                    return -20;
                }
            }
            Err(_) => return -2,
        }
        let chk = crate::fs::mount_move_check(&source_abs, &target_abs);
        if chk != 0 {
            return chk as isize;
        }
        // Relocate the whole mount subtree at the VFS layer (mount + all its
        // children), then update mount metadata + propagate to the new
        // parent's peers. A move must NOT propagate an unmount to the source's
        // peers.
        crate::fs::move_mount_subtree(&source_abs, &target_abs);
        // Moving onto a shared parent propagates the moved tree to the parent's
        // peers/slaves. Those copies must become REAL VFS mounts (对标
        // propagate_mnt), not just mountinfo rows, so their contents alias the
        // moved subtree. The source inode for each copy is the corresponding
        // node of the primary destination subtree (target_abs + rel), which
        // move_mount_subtree already relocated into the VFS.
        let propagated = crate::fs::mount_info_move(&source_abs, &target_abs);
        for (prop_path, rel, ns) in &propagated {
            let src_path = alloc::format!("{}{}", target_abs, rel);
            if let Ok(src_inode) = crate::fs::lookup(&src_path) {
                crate::fs::mount_in_ns(*ns, prop_path, crate::fs::bindfs::BindFs::new(src_inode));
            }
        }
        return 0;
    }

    // Normal mount
    if fstype.is_empty() { return -22; }
    if !is_known_fstype(&fstype) { return -19; }

    if source_ptr.is_null() && is_block_fs(&fstype) {
        return -22;
    }
    if !source.is_empty() && source != "none" {
        let source_abs = super::open::resolve_path(&source);
        if let Ok(src_inode) = crate::fs::lookup(&source_abs) {
            if src_inode.file_type() == crate::fs::FileType::CharDevice {
                return -15;
            }
            // BLKROSET-标记只读的块设备：RW 挂载必须 EACCES（对标 Linux
            // bdev_read_only → mount_bdev 返 -EACCES）；MS_RDONLY 挂载放行。
            // LTP ioctl04 设 loop 设备只读后据此校验。
            if flags & MS_RDONLY == 0 {
                if let Some((7, minor)) = src_inode.device_id() {
                    if crate::fs::devfs::loop_device_read_only(minor as usize) {
                        return -13; // EACCES
                    }
                }
            }
        }
    }

    match crate::fs::lookup(&target_abs) {
        Ok(inode) => {
            if inode.file_type() != crate::fs::FileType::Directory {
                return -20;
            }
        }
        Err(_) => return -2,
    }

    if crate::fs::is_mount_point(&target_abs) {
        return -16;
    }

    if target_abs != "/" {
        // Dispatch filesystem creation by type (per Linux fs/filesystems.c get_fs_type)
        let fs: alloc::sync::Arc<dyn crate::fs::FileSystem> = match fstype.as_str() {
            "sysfs" => crate::fs::sysfs::SysFs::new(),
            // cgroup v2 unified hierarchy. A fresh mount must expose the
            // cgroup.* / controller interface files (else LTP's
            // needs_cgroup_ctrls setup TBROKs opening cgroup.controllers);
            // cgroupfs serves them from the global CGROUP_TREE.
            "cgroup2" => crate::fs::cgroupfs::CgroupFs::new(),
            // cgroup v1: a separate hierarchy per controller. The `memory`
            // controller (named in the mount options or by a `.../memory`
            // target) is the real memcg hierarchy (memcgfs, delegating to
            // mm::memcg). Other v1 controllers reuse the v2 view.
            "cgroup" => {
                if data.split(',').any(|o| o == "memory")
                    || target_abs.ends_with("/memory")
                {
                    crate::fs::memcgfs::MemcgFs::new()
                } else {
                    crate::fs::cgroupfs::CgroupFs::new()
                }
            }
            // Block-device filesystems: back them with a persistent RamFs keyed
            // by (device, fstype) so umount + remount of the same device without
            // a reformat preserves content (fanotify10 mount_cycle). A real mkfs
            // drops the entry (device_fs_reset) so a reformat starts empty.
            _ if is_block_fs(&fstype) && source != "none" && !source.is_empty() => {
                device_fs_get_or_create(&source, &fstype)
            }
            // overlayfs: materialise the union of lowerdir+upperdir into a
            // RamFs so path-based watchers (inotify07/08) see the merged tree.
            "overlay" => build_overlay_fs(&data),
            _ => crate::fs::RamFs::new(),
        };
        crate::fs::mount(&target_abs, fs);
    }
    crate::fs::set_readonly_mount(&target_abs, flags & MS_RDONLY != 0);
    crate::fs::set_noexec_mount(&target_abs, flags & MS_NOEXEC != 0);
    crate::fs::set_nosuid_mount(&target_abs, flags & MS_NOSUID != 0);
    crate::fs::set_nodev_mount(&target_abs, flags & MS_NODEV != 0);
    crate::fs::set_noatime_mount(&target_abs, flags & MS_NOATIME != 0);
    crate::fs::set_nodiratime_mount(&target_abs, flags & MS_NODIRATIME != 0);
    crate::fs::set_nosymfollow_mount(&target_abs, flags & MS_NOSYMFOLLOW != 0);
    crate::fs::mount_info_add(&source, &target_abs, &fstype, flags);
    0
}

// ============================================================================
// umount2(2) — per Linux fs/namespace.c
// ============================================================================

/// 对标 Linux umount(2)：非 lazy(MNT_DETACH)/非强制(MNT_FORCE) 卸载时，若挂载
/// 子树仍被进程引用——任一进程持有位于该挂载点之下的打开文件描述符——内核必须
/// 返回 EBUSY。LTP umount02 case0：SAFE_CREAT(MNTPOINT "/file") 持有打开 fd，
/// 此时 umount(MNTPOINT) 应得 EBUSY；测试随后 SAFE_CLOSE(fd) 再卸载即成功。
///
/// FileDescriptor.path 存的是 open 时解析出的规范绝对路径，扫描全部进程
/// fd_table，若有打开文件路径以 `<target>/` 为前缀即视为子树被占用。
fn mount_subtree_has_open_files(target_abs: &str) -> bool {
    let prefix = alloc::format!("{}/", target_abs);
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(t) = table.as_ref() else { return false };
    for p in t.iter() {
        for slot in p.fd_table.iter() {
            if let Some(fd_obj) = slot {
                if let Some(ref path) = fd_obj.path {
                    if path.starts_with(&prefix) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

pub(in crate::syscall) fn sys_umount2(target: *const u8, flags: i32) -> isize {
    const MNT_FORCE: i32 = 0x1;
    const MNT_DETACH: i32 = 0x2;
    const MNT_EXPIRE: i32 = 0x4;
    const UMOUNT_NOFOLLOW: i32 = 0x8;
    const ALL: i32 = MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW;
    if flags & !ALL != 0 { return -22; }
    if (flags & MNT_EXPIRE != 0) && (flags & (MNT_FORCE | MNT_DETACH) != 0) {
        return -22;
    }
    let euid = current_euid();
    if euid != 0 { return -1; }
    if target.is_null() { return -14; }
    // 对标 Linux umount(2)：先 getname() 取路径——路径长度 > PATH_MAX 须返回
    // ENAMETOOLONG（-36），而非笼统 EFAULT。read_path 在前 PATH_MAX 字节无 NUL
    // 时返回 -36，未映射页返回 -14，正好区分两类错误。
    let target = match super::user::read_path(target) { Ok(s) => s, Err(e) => return e };
    if target.is_empty() { return -2; }
    let target_abs = super::open::resolve_path(&target);
    if target_abs == "/" { return -16; }
    match crate::fs::lookup(&target_abs) {
        Ok(inode) => {
            if inode.file_type() != crate::fs::FileType::Directory {
                return -22;
            }
        }
        Err(_) => return -2,
    }

    // 对标 Linux umount(2)：目标必须本身就是一个挂载点，否则返回 EINVAL。
    // LTP umount02 case3 用 "./"（解析为测试 cwd，非挂载点）期望 EINVAL；
    // 此检查须在 EBUSY 子树占用检查之前，否则 cwd 下挂载点里的打开 fd 会让
    // 非挂载点的 cwd 误判成 EBUSY。
    if !crate::fs::is_mount_point(&target_abs) {
        return -22; // -EINVAL
    }

    // 子树仍被占用（有进程打开位于该挂载点之下的文件）→ EBUSY，除非
    // MNT_DETACH 惰性卸载或 MNT_FORCE 强制卸载。LTP umount02 case0 依赖此语义。
    if flags & (MNT_DETACH | MNT_FORCE) == 0 && mount_subtree_has_open_files(&target_abs) {
        return -16; // -EBUSY
    }

    if flags & MNT_EXPIRE != 0 {
        let already_marked = crate::fs::mark_mount_expired(&target_abs);
        if !already_marked {
            return -11; // -EAGAIN
        }
    }

    crate::fs::inotify::INOTIFY_REGISTRY.notify_unmount(&target_abs);
    match crate::fs::unmount(&target_abs) {
        Ok(()) => {
            crate::fs::mount_info_remove(&target_abs);
            crate::fs::remove_expired_mark(&target_abs);
            0
        }
        Err(crate::fs::FsError::Busy) => -16,
        Err(crate::fs::FsError::InvalidArgument) => -22,
        Err(crate::fs::FsError::NotFound) => -2,
        Err(crate::fs::FsError::NotDirectory) => -22,
        Err(crate::fs::FsError::PermissionDenied) => -13,
        Err(_) => -5,
    }
}

// ============================================================================
// mount_setattr(2) — per Linux fs/namespace.c do_mount_setattr()
// ============================================================================

pub(in crate::syscall) fn sys_mount_setattr(
    dirfd: i32,
    path: *const u8,
    at_flags: u32,
    uattr: *const u8,
    usize_: usize,
) -> isize {
    const AT_EMPTY_PATH: u32 = 0x1000;
    const AT_RECURSIVE: u32 = 0x8000;
    const MOUNT_ATTR_RDONLY: u64 = 0x01;
    const MOUNT_ATTR_NOSUID: u64 = 0x02;
    const MOUNT_ATTR_NODEV: u64 = 0x04;
    const MOUNT_ATTR_NOEXEC: u64 = 0x08;
    const MOUNT_ATTR_NODIRATIME: u64 = 0x80;
    const MOUNT_ATTR_NOSYMFOLLOW: u64 = 0x200000;
    const MS_SHARED_V: u64 = 1 << 20;
    const MS_SLAVE_V: u64 = 1 << 19;
    const MS_UNBINDABLE_V: u64 = 1 << 17;

    let euid = current_euid();
    if euid != 0 { return -1; }
    if path.is_null() { return -14; }
    let path_str = match read_c_string(path) { Ok(s) => s, Err(_) => return -14 };

    // open_tree(OPEN_TREE_CLONE) returns a detached mount fd; mount_setattr on
    // it tunes the pending mount's attributes (对标 Linux: attr change on a
    // mount not yet attached). move_mount(2) later applies them on graft.
    if path_str.is_empty() && at_flags & AT_EMPTY_PATH != 0 && dirfd >= 0 {
        let mut pend = PENDING_MOUNTS.lock();
        if let Some(pm) = pend.get_mut(&dirfd) {
            if !uattr.is_null() && usize_ >= 8 {
                let len = usize_.min(32);
                if let Ok(buf) = crate::syscall::user::read_user_buffer(uattr, len) {
                    let attr_set = if buf.len() >= 8 {
                        u64::from_le_bytes(buf[0..8].try_into().unwrap_or([0;8]))
                    } else { 0 };
                    let attr_clr = if buf.len() >= 16 {
                        u64::from_le_bytes(buf[8..16].try_into().unwrap_or([0;8]))
                    } else { 0 };
                    let map: [(u64, u32); 7] = [
                        (MOUNT_ATTR_RDONLY, 0x01),
                        (MOUNT_ATTR_NOSUID, 0x02),
                        (MOUNT_ATTR_NODEV, 0x04),
                        (MOUNT_ATTR_NOEXEC, 0x08),
                        (0x10, 0x10), // MOUNT_ATTR_NOATIME
                        (MOUNT_ATTR_NODIRATIME, 0x80),
                        (MOUNT_ATTR_NOSYMFOLLOW, 0x200000),
                    ];
                    for (msk, bit) in map {
                        if attr_set & msk != 0 { pm.attr_flags |= bit; }
                        if attr_clr & msk != 0 { pm.attr_flags &= !bit; }
                    }
                }
            }
            return 0;
        }
    }

    let path_abs = if path_str.is_empty() && at_flags & AT_EMPTY_PATH != 0 {
        if dirfd < 0 { return -9; }
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        let pid = match t.current { Some(p) => p, None => return -3 };
        let process = match t.get_fd_process(pid) { Some(p) => p, None => return -3 };
        match crate::fs::fd::get(process, dirfd as usize) {
            Some(fd_obj) => fd_obj.path.clone().unwrap_or_default(),
            None => return -9,
        }
    } else if path_str.is_empty() {
        return -2;
    } else {
        super::open::resolve_path(&path_str)
    };

    let mount_target = {
        let table = crate::fs::MOUNT_INFO_TABLE.lock();
        let normalized = if path_abs.is_empty() { "/" } else { path_abs.as_str() };
        table.iter()
            .filter(|m| {
                if m.target == "/" { return true; }
                if normalized == m.target { return true; }
                normalized.len() > m.target.len()
                    && normalized.as_bytes().get(m.target.len()) == Some(&b'/')
                    && normalized.starts_with(&m.target)
            })
            .max_by_key(|m| m.target.len())
            .map(|m| m.target.clone())
    };
    let mount_target = match mount_target {
        Some(t) => t,
        None => return -22,
    };

    if !uattr.is_null() && usize_ >= 8 {
        let len = usize_.min(32);
        if let Ok(buf) = crate::syscall::user::read_user_buffer(uattr, len) {
            let attr_set = if buf.len() >= 8 {
                u64::from_le_bytes(buf[0..8].try_into().unwrap_or([0;8]))
            } else { 0 };
            let attr_clr = if buf.len() >= 16 {
                u64::from_le_bytes(buf[8..16].try_into().unwrap_or([0;8]))
            } else { 0 };
            let mut table = crate::fs::MOUNT_INFO_TABLE.lock();
            if let Some(m) = table.iter_mut().find(|m| m.target == mount_target) {
                if attr_set & MOUNT_ATTR_RDONLY != 0 { m.flags |= 0x01; }
                if attr_set & MOUNT_ATTR_NOSUID != 0 { m.flags |= 0x02; }
                if attr_set & MOUNT_ATTR_NODEV != 0 { m.flags |= 0x04; }
                if attr_set & MOUNT_ATTR_NOEXEC != 0 { m.flags |= 0x08; }
                if attr_set & MOUNT_ATTR_NODIRATIME != 0 { m.flags |= 0x800; }
                if attr_set & MOUNT_ATTR_NOSYMFOLLOW != 0 { m.flags |= 0x100000; }
                if attr_clr & MOUNT_ATTR_RDONLY != 0 { m.flags &= !0x01; }
                if attr_clr & MOUNT_ATTR_NOSUID != 0 { m.flags &= !0x02; }
                if attr_clr & MOUNT_ATTR_NODEV != 0 { m.flags &= !0x04; }
                if attr_clr & MOUNT_ATTR_NOEXEC != 0 { m.flags &= !0x08; }
                if attr_clr & MOUNT_ATTR_NODIRATIME != 0 { m.flags &= !0x800; }
                if attr_clr & MOUNT_ATTR_NOSYMFOLLOW != 0 { m.flags &= !0x100000; }
            }
            // Update VFS mount flags
            if attr_set & MOUNT_ATTR_RDONLY != 0 {
                crate::fs::set_readonly_mount(&mount_target, true);
            } else if attr_clr & MOUNT_ATTR_RDONLY != 0 {
                crate::fs::set_readonly_mount(&mount_target, false);
            }
            if attr_set & MOUNT_ATTR_NOEXEC != 0 {
                crate::fs::set_noexec_mount(&mount_target, true);
            } else if attr_clr & MOUNT_ATTR_NOEXEC != 0 {
                crate::fs::set_noexec_mount(&mount_target, false);
            }
            if attr_set & MOUNT_ATTR_NOSUID != 0 {
                crate::fs::set_nosuid_mount(&mount_target, true);
            } else if attr_clr & MOUNT_ATTR_NOSUID != 0 {
                crate::fs::set_nosuid_mount(&mount_target, false);
            }
            if attr_set & MOUNT_ATTR_NODEV != 0 {
                crate::fs::set_nodev_mount(&mount_target, true);
            } else if attr_clr & MOUNT_ATTR_NODEV != 0 {
                crate::fs::set_nodev_mount(&mount_target, false);
            }
            // Handle propagation field at offset 24 (对标 Linux mount_attr.propagation)
            let propagation = if buf.len() >= 32 {
                u64::from_le_bytes(buf[24..32].try_into().unwrap_or([0;8]))
            } else { 0 };
            if propagation != 0 {
                let recursive = at_flags & AT_RECURSIVE != 0;
                let prop = if propagation & MS_SHARED_V != 0 {
                    crate::fs::mount::MountPropagation::Shared
                } else if propagation & MS_SLAVE_V != 0 {
                    crate::fs::mount::MountPropagation::Slave
                } else if propagation & MS_UNBINDABLE_V != 0 {
                    crate::fs::mount::MountPropagation::Unbindable
                } else {
                    crate::fs::mount::MountPropagation::Private
                };
                crate::fs::mount::set_mount_propagation(&mount_target, prop, recursive);
            }
        }
    }
    0
}

// ============================================================================
// fsopen(2) — per Linux fs/fsopen.c
// ============================================================================

pub(in crate::syscall) fn sys_fsopen(fs_name: *const u8, flags: u32) -> isize {
    const FSOPEN_CLOEXEC: u32 = 1;
    if flags & !FSOPEN_CLOEXEC != 0 { return -22; }

    let name = match super::user::read_c_string(fs_name) {
        Ok(s) => s,
        Err(_) => return -14,
    };

    let known_fs = ["tmpfs", "ramfs", "proc", "sysfs", "devtmpfs", "ext4", "ext2", "ext3",
                    "vfat", "fat", "btrfs", "xfs", "nfs", "cifs", "overlay"];
    if !known_fs.iter().any(|&f| f == name.as_str()) {
        return -19;
    }

    let fd = alloc_anon_fd("anon_inode:[fscontext]");
    if fd < 0 { return -24; }

    FS_CONTEXTS.lock().insert(fd, FsContext {
        fs_name: name,
        source: None,
        options: Vec::new(),
        mounted: false,
        legacy_size: 0,
    });

    fd as isize
}

// ============================================================================
// fsconfig(2) — per Linux fs/fsopen.c vfs_fsconfig_locked()
// ============================================================================

pub(in crate::syscall) fn sys_fsconfig(fd: i32, cmd: u32, key: *const u8, value: *const u8, aux: i32) -> isize {
    const FSCONFIG_SET_FLAG: u32 = 0;
    const FSCONFIG_SET_STRING: u32 = 1;
    const FSCONFIG_SET_BINARY: u32 = 2;
    const FSCONFIG_SET_PATH: u32 = 3;
    const FSCONFIG_SET_PATH_EMPTY: u32 = 4;
    const FSCONFIG_SET_FD: u32 = 5;
    const FSCONFIG_CMD_CREATE: u32 = 6;
    const FSCONFIG_CMD_RECONFIGURE: u32 = 7;

    // Linux legacy_parse_param() accumulates every SET_FLAG/SET_STRING option
    // into a single page-sized buffer and rejects (EINVAL) once the cumulative
    // length would exceed one page. Without this cap an attacker could overflow
    // it (CVE-2022-0185); LTP fsconfig03 verifies the cap fires.
    const LEGACY_PAGE: usize = 4096;

    let mut contexts = FS_CONTEXTS.lock();
    let ctx = match contexts.get_mut(&fd) {
        Some(c) => c,
        None => return -22,
    };

    if cmd > FSCONFIG_CMD_RECONFIGURE { return -95; }

    match cmd {
        FSCONFIG_SET_FLAG => {
            if key.is_null() { return -22; }
            if !value.is_null() { return -22; }
            if aux != 0 { return -22; }
            let k = match super::user::read_c_string(key) { Ok(s) => s, Err(_) => return -14 };
            // legacy_parse_param: len = strlen(key); reject if len > PAGE-2-size.
            let sep = if ctx.legacy_size > 0 { 1 } else { 0 };
            let cur = ctx.legacy_size + sep;
            if k.len() > LEGACY_PAGE.saturating_sub(2).saturating_sub(cur) { return -22; }
            ctx.legacy_size = cur + k.len();
            ctx.options.push((k, String::new()));
            0
        }
        FSCONFIG_SET_STRING => {
            if key.is_null() { return -22; }
            if value.is_null() { return -22; }
            if aux != 0 { return -22; }
            let k = match super::user::read_c_string(key) { Ok(s) => s, Err(_) => return -14 };
            let v = match super::user::read_c_string(value) { Ok(s) => s, Err(_) => return -14 };
            if k == "source" {
                ctx.source = Some(v);
            } else {
                // legacy_parse_param: string len = strlen(key)+1+strlen(value).
                let sep = if ctx.legacy_size > 0 { 1 } else { 0 };
                let cur = ctx.legacy_size + sep;
                let len = k.len() + 1 + v.len();
                if len > LEGACY_PAGE.saturating_sub(2).saturating_sub(cur) { return -22; }
                ctx.legacy_size = cur + len;
                ctx.options.push((k, v));
            }
            0
        }
        FSCONFIG_SET_BINARY => {
            if key.is_null() { return -22; }
            if value.is_null() { return -22; }
            if aux <= 0 { return -22; }
            0
        }
        FSCONFIG_SET_PATH | FSCONFIG_SET_PATH_EMPTY => {
            if key.is_null() { return -22; }
            if value.is_null() { return -22; }
            if aux < 0 && aux != -100 { return -22; }
            0
        }
        FSCONFIG_SET_FD => {
            if key.is_null() { return -22; }
            if !value.is_null() { return -22; }
            if aux < 0 { return -22; }
            0
        }
        FSCONFIG_CMD_CREATE | FSCONFIG_CMD_RECONFIGURE => {
            if !key.is_null() { return -22; }
            if !value.is_null() { return -22; }
            if aux != 0 { return -22; }
            if cmd == FSCONFIG_CMD_CREATE { ctx.mounted = false; }
            0
        }
        _ => -95,
    }
}

// ============================================================================
// fsmount(2) — per Linux fs/namespace.c
// ============================================================================

pub(in crate::syscall) fn sys_fsmount(fs_fd: i32, flags: u32, attr_flags: u32) -> isize {
    const FSMOUNT_CLOEXEC: u32 = 1;
    if flags & !FSMOUNT_CLOEXEC != 0 { return -22; }
    const MOUNT_ATTR_RDONLY: u32 = 0x01;
    const MOUNT_ATTR_NOSUID: u32 = 0x02;
    const MOUNT_ATTR_NODEV: u32 = 0x04;
    const MOUNT_ATTR_NOEXEC: u32 = 0x08;
    const MOUNT_ATTR_NOATIME: u32 = 0x10;
    const MOUNT_ATTR_STRICTATIME: u32 = 0x20;
    const MOUNT_ATTR_NODIRATIME: u32 = 0x80;
    const MOUNT_ATTR_IDMAP: u32 = 0x100000;
    const MOUNT_ATTR_NOSYMFOLLOW: u32 = 0x200000;
    let valid_attrs = MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV
        | MOUNT_ATTR_NOEXEC | MOUNT_ATTR_NOATIME | MOUNT_ATTR_STRICTATIME
        | MOUNT_ATTR_NODIRATIME | MOUNT_ATTR_IDMAP | MOUNT_ATTR_NOSYMFOLLOW;
    if attr_flags & !valid_attrs != 0 { return -22; }

    // Snapshot the configured filesystem context. fsmount produces a detached
    // mount whose fs/source/options are fixed at this point; the actual graft
    // happens later in move_mount(2).
    let (fs_name, source, data) = {
        let contexts = FS_CONTEXTS.lock();
        let ctx = match contexts.get(&fs_fd) {
            Some(c) => c,
            None => return -9,
        };
        let data = ctx.options.iter()
            .map(|(k, v)| if v.is_empty() { k.clone() } else { alloc::format!("{}={}", k, v) })
            .collect::<Vec<_>>()
            .join(",");
        let source = ctx.source.clone().unwrap_or_else(|| String::from("none"));
        (ctx.fs_name.clone(), source, data)
    };

    let mount_fd = alloc_anon_fd("anon_inode:[mnt]");
    if mount_fd < 0 { return -24; }

    {
        let mut contexts = FS_CONTEXTS.lock();
        if let Some(ctx) = contexts.get_mut(&fs_fd) { ctx.mounted = true; }
    }

    PENDING_MOUNTS.lock().insert(mount_fd, PendingMount {
        fs_name,
        source,
        data,
        attr_flags,
    });

    mount_fd as isize
}

// ============================================================================
// fspick(2) — per Linux fs/fsopen.c
// ============================================================================

pub(in crate::syscall) fn sys_fspick(dfd: i32, path: *const u8, flags: u32) -> isize {
    const FSPICK_CLOEXEC: u32 = 1;
    const FSPICK_SYMLINK_NOFOLLOW: u32 = 2;
    const FSPICK_NO_AUTOMOUNT: u32 = 4;
    const FSPICK_EMPTY_PATH: u32 = 8;
    let valid_flags = FSPICK_CLOEXEC | FSPICK_SYMLINK_NOFOLLOW | FSPICK_NO_AUTOMOUNT | FSPICK_EMPTY_PATH;
    if flags & !valid_flags != 0 { return -22; }

    let p = if !path.is_null() {
        match super::user::read_c_string(path) { Ok(s) => s, Err(_) => return -14 }
    } else {
        String::new()
    };

    const AT_FDCWD: i32 = -100;
    if dfd != AT_FDCWD && dfd >= 0 {
        let valid_fd = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| t.current.and_then(|pid| {
                t.get(pid).map(|p| (dfd as usize) < p.fd_table.len() && p.fd_table[dfd as usize].is_some())
            })).unwrap_or(false)
        };
        if !valid_fd { return -9; }
    } else if dfd != AT_FDCWD {
        return -9;
    }

    let full = match super::open::resolve_at_path(dfd as isize, &p) {
        Ok(s) => s,
        Err(_) => p.clone(),
    };
    if crate::fs::lookup(&full).is_err() && !full.is_empty() {
        return -2;
    }

    let fd = alloc_anon_fd("anon_inode:[fscontext]");
    if fd < 0 { return -24; }

    FS_CONTEXTS.lock().insert(fd, FsContext {
        fs_name: String::from("tmpfs"),
        source: Some(full),
        options: Vec::new(),
        mounted: false,
        legacy_size: 0,
    });

    fd as isize
}

// ============================================================================
// open_tree(2) — per Linux fs/namespace.c
// ============================================================================

pub(in crate::syscall) fn sys_open_tree(dfd: i32, path: *const u8, flags: u32) -> isize {
    const OPEN_TREE_CLONE: u32 = 1;
    const OPEN_TREE_CLOEXEC: u32 = 0o2000000;
    const AT_EMPTY_PATH: u32 = 0x1000;
    const AT_RECURSIVE: u32 = 0x8000;
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_FDCWD: i32 = -100;
    let valid = OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH | AT_RECURSIVE | AT_SYMLINK_NOFOLLOW;
    if flags & !valid != 0 { return -22; }

    let p = if !path.is_null() {
        match super::user::read_c_string(path) { Ok(s) => s, Err(_) => return -14 }
    } else {
        String::new()
    };

    if dfd != AT_FDCWD && dfd >= 0 {
        let valid_fd = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| t.current.and_then(|pid| {
                t.get(pid).map(|p| (dfd as usize) < p.fd_table.len() && p.fd_table[dfd as usize].is_some())
            })).unwrap_or(false)
        };
        if !valid_fd { return -9; }
    } else if dfd != AT_FDCWD {
        return -9;
    }

    let resolved = if flags & AT_EMPTY_PATH != 0 && p.is_empty() {
        if dfd >= 0 {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| t.current.and_then(|pid| {
                t.get(pid).and_then(|proc| {
                    crate::fs::fd::get(proc, dfd as usize).and_then(|fd_obj| fd_obj.path.clone())
                })
            })).unwrap_or_default()
        } else {
            String::from("/")
        }
    } else if !p.is_empty() {
        let full = match super::open::resolve_at_path(dfd as isize, &p) {
            Ok(s) => s,
            Err(_) => p.clone(),
        };
        if crate::fs::lookup(&full).is_err() {
            return -2;
        }
        full
    } else {
        String::from("/")
    };

    // OPEN_TREE_CLONE is like bind mount — check unbindable (对标 Linux)
    if flags & OPEN_TREE_CLONE != 0 {
        let mt = crate::fs::MOUNT_INFO_TABLE.lock();
        if let Some(m) = mt.iter().find(|m| m.target == resolved) {
            if m.propagation == crate::fs::MountPropagation::Unbindable {
                return -22; // EINVAL: cannot clone unbindable mount
            }
        }
    }

    // open_tree(2) fds are FMODE_PATH on Linux (no struct file for I/O); mark
    // them O_PATH so accept()/read()/... reject them with EBADF (accept03).
    const O_PATH: u32 = 0o10000000;
    let fd = alloc_anon_fd_flags("anon_inode:[mnt]", O_PATH);
    if fd < 0 { return -24; }

    // OPEN_TREE_CLONE detaches a copy of the subtree's mount (对标 Linux
    // copy_tree). Record it as a pending mount so that a later mount_setattr(2)
    // can tune its attributes and move_mount(2) can graft it elsewhere.
    if flags & OPEN_TREE_CLONE != 0 {
        const MS_RDONLY: usize = 1;
        const MS_NOSUID: usize = 2;
        const MS_NODEV: usize = 4;
        const MS_NOEXEC: usize = 8;
        const MS_NOATIME: usize = 1024;
        const MS_NODIRATIME: usize = 2048;
        let (fs_name, source, attr_flags) = {
            let mt = crate::fs::MOUNT_INFO_TABLE.lock();
            mt.iter()
                .filter(|m| m.target == resolved || (resolved.len() > m.target.len()
                    && resolved.as_bytes().get(m.target.len()) == Some(&b'/')
                    && resolved.starts_with(&m.target))
                    || m.target == "/")
                .max_by_key(|m| m.target.len())
                .map(|m| {
                    let mut a = 0u32;
                    if m.flags & MS_RDONLY != 0 { a |= 0x01; }
                    if m.flags & MS_NOSUID != 0 { a |= 0x02; }
                    if m.flags & MS_NODEV != 0 { a |= 0x04; }
                    if m.flags & MS_NOEXEC != 0 { a |= 0x08; }
                    if m.flags & MS_NOATIME != 0 { a |= 0x10; }
                    if m.flags & MS_NODIRATIME != 0 { a |= 0x80; }
                    (m.fstype.clone(), m.source.clone(), a)
                })
                .unwrap_or_else(|| (String::from("tmpfs"), String::from("none"), 0))
        };
        PENDING_MOUNTS.lock().insert(fd, PendingMount {
            fs_name,
            source,
            data: String::new(),
            attr_flags,
        });
    }

    fd as isize
}

// ============================================================================
// move_mount(2) — per Linux fs/namespace.c
// ============================================================================

pub(in crate::syscall) fn sys_move_mount(
    from_dfd: i32, from_path: *const u8,
    _to_dfd: i32, to_path: *const u8,
    flags: u32
) -> isize {
    const MOVE_MOUNT_F_SYMLINKS: u32 = 0x01;
    const MOVE_MOUNT_F_AUTOMOUNTS: u32 = 0x02;
    const MOVE_MOUNT_F_EMPTY_PATH: u32 = 0x04;
    const MOVE_MOUNT_T_SYMLINKS: u32 = 0x10;
    const MOVE_MOUNT_T_AUTOMOUNTS: u32 = 0x20;
    const MOVE_MOUNT_T_EMPTY_PATH: u32 = 0x40;
    const MOVE_MOUNT_SET_GROUP: u32 = 0x100;
    const MOVE_MOUNT_BENEATH: u32 = 0x200;
    let valid = MOVE_MOUNT_F_SYMLINKS | MOVE_MOUNT_F_AUTOMOUNTS | MOVE_MOUNT_F_EMPTY_PATH
        | MOVE_MOUNT_T_SYMLINKS | MOVE_MOUNT_T_AUTOMOUNTS | MOVE_MOUNT_T_EMPTY_PATH
        | MOVE_MOUNT_SET_GROUP | MOVE_MOUNT_BENEATH;
    if flags & !valid != 0 { return -22; }

    const AT_FDCWD: i32 = -100;

    let from = if !from_path.is_null() {
        match super::user::read_c_string(from_path) { Ok(s) => s, Err(_) => return -14 }
    } else {
        String::new()
    };
    let to = if !to_path.is_null() {
        match super::user::read_c_string(to_path) { Ok(s) => s, Err(_) => return -14 }
    } else {
        String::new()
    };

    // ---- resolve the SOURCE (what is being moved) ----
    // Empty from_path => the mount is referenced directly by from_dfd
    // (MOVE_MOUNT_F_EMPTY_PATH): typically an fd returned by fsmount(2).
    let pending: Option<PendingMount>;
    let from_existing: Option<String>;
    if from.is_empty() {
        if !fd_is_valid(from_dfd) { return -9; } // EBADF
        pending = PENDING_MOUNTS.lock().remove(&from_dfd);
        from_existing = None;
        if pending.is_none() {
            // A valid fd that is not a detached fsmount: moving an already
            // attached mount by fd is not modelled here.
            return -22;
        }
    } else {
        if from_dfd != AT_FDCWD && !fd_is_valid(from_dfd) { return -9; } // EBADF
        let full = match super::open::resolve_at_path(from_dfd as isize, &from) {
            Ok(s) => s,
            Err(_) => from.clone(),
        };
        if crate::fs::lookup(&full).is_err() { return -2; } // ENOENT
        if !crate::fs::is_mount_point(&full) { return -22; } // EINVAL: not a mount
        pending = None;
        from_existing = Some(full);
    }

    // ---- resolve the DESTINATION mount point ----
    let target_abs = if to.is_empty() {
        if !fd_is_valid(_to_dfd) { return -9; } // EBADF
        let p = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| t.current.and_then(|pid| {
                t.get(pid).and_then(|proc| {
                    crate::fs::fd::get(proc, _to_dfd as usize).and_then(|fd_obj| fd_obj.path.clone())
                })
            }))
        };
        match p { Some(s) => s, None => return -9 }
    } else {
        if _to_dfd != AT_FDCWD && !fd_is_valid(_to_dfd) { return -9; } // EBADF
        match super::open::resolve_at_path(_to_dfd as isize, &to) {
            Ok(s) => s,
            Err(_) => to.clone(),
        }
    };
    match crate::fs::lookup(&target_abs) {
        Ok(inode) => {
            if inode.file_type() != crate::fs::FileType::Directory { return -20; } // ENOTDIR
        }
        Err(_) => return -2, // ENOENT
    }
    if crate::fs::is_mount_point(&target_abs) { return -16; } // EBUSY

    const MS_RDONLY: usize = 1;
    const MS_NOSUID: usize = 2;
    const MS_NODEV: usize = 4;
    const MS_NOEXEC: usize = 8;
    const MS_NOSYMFOLLOW: usize = 0x100;
    const MS_NOATIME: usize = 1024;
    const MS_NODIRATIME: usize = 2048;
    const MOUNT_ATTR_RDONLY: u32 = 0x01;
    const MOUNT_ATTR_NOSUID: u32 = 0x02;
    const MOUNT_ATTR_NODEV: u32 = 0x04;
    const MOUNT_ATTR_NOEXEC: u32 = 0x08;
    const MOUNT_ATTR_NOATIME: u32 = 0x10;
    const MOUNT_ATTR_NODIRATIME: u32 = 0x80;
    const MOUNT_ATTR_NOSYMFOLLOW: u32 = 0x200000;

    if let Some(pm) = pending {
        // Graft a freshly created (detached) filesystem onto the target.
        let fs = create_fs_for_mount(&pm.fs_name, &pm.source, &pm.data, &target_abs);
        crate::fs::mount(&target_abs, fs);
        let a = pm.attr_flags;
        crate::fs::set_readonly_mount(&target_abs, a & MOUNT_ATTR_RDONLY != 0);
        crate::fs::set_nosuid_mount(&target_abs, a & MOUNT_ATTR_NOSUID != 0);
        crate::fs::set_nodev_mount(&target_abs, a & MOUNT_ATTR_NODEV != 0);
        crate::fs::set_noexec_mount(&target_abs, a & MOUNT_ATTR_NOEXEC != 0);
        crate::fs::set_noatime_mount(&target_abs, a & MOUNT_ATTR_NOATIME != 0);
        crate::fs::set_nodiratime_mount(&target_abs, a & MOUNT_ATTR_NODIRATIME != 0);
        let mut ms_flags = 0usize;
        if a & MOUNT_ATTR_RDONLY != 0 { ms_flags |= MS_RDONLY; }
        if a & MOUNT_ATTR_NOSUID != 0 { ms_flags |= MS_NOSUID; }
        if a & MOUNT_ATTR_NODEV != 0 { ms_flags |= MS_NODEV; }
        if a & MOUNT_ATTR_NOEXEC != 0 { ms_flags |= MS_NOEXEC; }
        if a & MOUNT_ATTR_NOATIME != 0 { ms_flags |= MS_NOATIME; }
        if a & MOUNT_ATTR_NODIRATIME != 0 { ms_flags |= MS_NODIRATIME; }
        if a & MOUNT_ATTR_NOSYMFOLLOW != 0 { ms_flags |= MS_NOSYMFOLLOW; }
        crate::fs::mount_info_add(&pm.source, &target_abs, &pm.fs_name, ms_flags);
        return 0;
    }

    // Move an already-attached mount subtree (对标 do_move_mount).
    if let Some(source_abs) = from_existing {
        let chk = crate::fs::mount_move_check(&source_abs, &target_abs);
        if chk != 0 { return chk as isize; }
        crate::fs::move_mount_subtree(&source_abs, &target_abs);
        let propagated = crate::fs::mount_info_move(&source_abs, &target_abs);
        for (prop_path, rel, ns) in &propagated {
            let src_path = alloc::format!("{}{}", target_abs, rel);
            if let Ok(src_inode) = crate::fs::lookup(&src_path) {
                crate::fs::mount_in_ns(*ns, prop_path, crate::fs::bindfs::BindFs::new(src_inode));
            }
        }
        return 0;
    }

    -22
}

// ============================================================================
// statmount(2) / listmount(2) — Linux 6.8+ mount information queries
// ============================================================================

pub(in crate::syscall) fn sys_statmount(_req: *const u8, _buf: *mut u8, _bufsize: usize, _flags: u32) -> isize {
    -38 // ENOSYS — not yet implemented
}

pub(in crate::syscall) fn sys_listmount(_req: *const u8, _mnt_ids: *mut u64, _nr_mnt_ids: usize, _flags: u32) -> isize {
    -38 // ENOSYS — not yet implemented
}
