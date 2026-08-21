use super::SysResult;
use crate::{user::UserTaskContainer, utils::useref::UserRef};
use core::sync::atomic::{AtomicU64, Ordering};
use executor::tid2task;
use fs::{dentry::is_readonly_path, file::File, pathbuf::PathBuf};
use libc_types::{
    fcntl::{OpenFlags, AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW},
    resource::Rlimit,
    types::{Stat, StatMode},
    utsname::UTSname,
};
use log::{debug, warn};
use syscalls::Errno;
use vfscore::FileType;

const GRND_NONBLOCK: usize = 0x0001;
const GRND_RANDOM: usize = 0x0002;
const GETRANDOM_FLAGS: usize = GRND_NONBLOCK | GRND_RANDOM;
const PRIO_PROCESS: isize = 0;
const PRIO_PGRP: isize = 1;
const PRIO_USER: isize = 2;
const MODE_PERMISSIONS: u32 = 0o7777;
static GETRANDOM_SEED: AtomicU64 = AtomicU64::new(0xdead_beef_cafe_babe);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct KernelSysInfo {
    uptime: isize,
    loads: [usize; 3],
    totalram: usize,
    freeram: usize,
    sharedram: usize,
    bufferram: usize,
    totalswap: usize,
    freeswap: usize,
    procs: u16,
    pad: u16,
    totalhigh: usize,
    freehigh: usize,
    mem_unit: u32,
    padding: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct UserCapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct UserCapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

impl UserTaskContainer {
    pub fn sys_uname(&self, uts_ptr: UserRef<UTSname>) -> SysResult {
        debug!("sys_uname @ uts_ptr: {}", uts_ptr);

        // for linux app compatible
        let sys_name = b"Linux";
        let sys_nodename = b"debian";
        let sys_release = b"5.10.0-7-riscv64";
        let sys_version = b"#1 SMP Debian 5.10.40-1 (2021-05-28)";
        let sys_machine = b"riscv qemu";
        let sys_domain = b"";

        uts_ptr.with_mut(|uts| {
            uts.sysname[..sys_name.len()].copy_from_slice(sys_name);
            uts.nodename[..sys_nodename.len()].copy_from_slice(sys_nodename);
            uts.release[..sys_release.len()].copy_from_slice(sys_release);
            uts.version[..sys_version.len()].copy_from_slice(sys_version);
            uts.machine[..sys_machine.len()].copy_from_slice(sys_machine);
            uts.domainname[..sys_domain.len()].copy_from_slice(sys_domain);
        });
        Ok(0)
    }

    /// TODO: FINISH sys_getrlimit
    pub fn sys_prlimit64(
        &self,
        pid: usize,
        resource: usize,
        new_limit: UserRef<Rlimit>,
        old_limit: UserRef<Rlimit>,
    ) -> SysResult {
        debug!(
            "sys_getrlimit @ pid: {}, resource: {}, new_limit: {}, old_limit: {}",
            pid, resource, new_limit, old_limit
        );
        match resource {
            1 | 7 => {
                if new_limit.is_valid() {
                    let rlimit = new_limit.read();
                    self.task.pcb.lock().rlimits[resource] = rlimit.curr;
                }
                if old_limit.is_valid() {
                    old_limit.with_mut(|rlimit| {
                        rlimit.max = self.task.inner_map(|inner| inner.rlimits[resource]);
                        rlimit.curr = rlimit.max;
                    })
                }
            }
            _ => {
                warn!("need to finish prlimit64: resource {}", resource)
            }
        }
        Ok(0)
    }

    pub fn sys_geteuid(&self) -> SysResult {
        Ok(self.task.pcb.lock().credentials.euid as usize)
    }

    pub fn sys_getegid(&self) -> SysResult {
        Ok(self.task.pcb.lock().credentials.egid as usize)
    }

    pub fn sys_getgid(&self) -> SysResult {
        Ok(self.task.pcb.lock().credentials.rgid as usize)
    }

    pub fn sys_getuid(&self) -> SysResult {
        Ok(self.task.pcb.lock().credentials.ruid as usize)
    }

    pub fn sys_getresuid(
        &self,
        ruid: UserRef<u32>,
        euid: UserRef<u32>,
        suid: UserRef<u32>,
    ) -> SysResult {
        let credentials = self.task.pcb.lock().credentials;
        if ruid.is_valid() {
            ruid.write(credentials.ruid);
        }
        if euid.is_valid() {
            euid.write(credentials.euid);
        }
        if suid.is_valid() {
            suid.write(credentials.suid);
        }
        Ok(0)
    }

    pub fn sys_getresgid(
        &self,
        rgid: UserRef<u32>,
        egid: UserRef<u32>,
        sgid: UserRef<u32>,
    ) -> SysResult {
        let credentials = self.task.pcb.lock().credentials;
        if rgid.is_valid() {
            rgid.write(credentials.rgid);
        }
        if egid.is_valid() {
            egid.write(credentials.egid);
        }
        if sgid.is_valid() {
            sgid.write(credentials.sgid);
        }
        Ok(0)
    }

    pub fn sys_getgroups(&self, _size: usize, _list: UserRef<u32>) -> SysResult {
        Ok(0)
    }

    pub fn sys_getpriority(&self, which: usize, who: usize) -> SysResult {
        let which = which as isize;
        let who = who as isize;

        if !matches!(which, PRIO_PROCESS | PRIO_PGRP | PRIO_USER) {
            return Err(Errno::EINVAL);
        }
        if who < 0 {
            return Err(Errno::ESRCH);
        }

        match which {
            PRIO_PROCESS => {
                let who = who as usize;
                if who != 0 && who != self.task.process_id && who != self.tid {
                    return Err(Errno::ESRCH);
                }
            }
            PRIO_PGRP => {
                if who != 0 {
                    return Err(Errno::ESRCH);
                }
            }
            PRIO_USER => {
                let uid = self.task.pcb.lock().credentials.ruid;
                if who != 0 && who as u32 != uid {
                    return Err(Errno::ESRCH);
                }
            }
            _ => unreachable!(),
        }

        // Linux getpriority(2) returns 20 - nice at the raw syscall layer.
        Ok(20)
    }

    fn can_set_id(current: u32, saved: u32, real: u32, new_id: u32) -> bool {
        current == 0 || new_id == current || new_id == saved || new_id == real
    }

    pub fn sys_setuid(&self, uid: u32) -> SysResult {
        let mut pcb = self.task.pcb.lock();
        if pcb.credentials.euid == 0 {
            pcb.credentials.ruid = uid;
            pcb.credentials.euid = uid;
            pcb.credentials.suid = uid;
            return Ok(0);
        }
        if Self::can_set_id(
            pcb.credentials.euid,
            pcb.credentials.suid,
            pcb.credentials.ruid,
            uid,
        ) {
            pcb.credentials.euid = uid;
            Ok(0)
        } else {
            Err(Errno::EPERM)
        }
    }

    pub fn sys_setgid(&self, gid: u32) -> SysResult {
        let mut pcb = self.task.pcb.lock();
        if pcb.credentials.egid == 0 {
            pcb.credentials.rgid = gid;
            pcb.credentials.egid = gid;
            pcb.credentials.sgid = gid;
            return Ok(0);
        }
        if Self::can_set_id(
            pcb.credentials.egid,
            pcb.credentials.sgid,
            pcb.credentials.rgid,
            gid,
        ) {
            pcb.credentials.egid = gid;
            Ok(0)
        } else {
            Err(Errno::EPERM)
        }
    }

    pub fn sys_setreuid(&self, ruid: u32, euid: u32) -> SysResult {
        const NO_CHANGE: u32 = u32::MAX;

        let mut pcb = self.task.pcb.lock();
        let old = pcb.credentials;
        let new_ruid = if ruid == NO_CHANGE { old.ruid } else { ruid };
        let new_euid = if euid == NO_CHANGE { old.euid } else { euid };

        if old.euid != 0
            && (!Self::can_set_id(old.euid, old.suid, old.ruid, new_ruid)
                || !Self::can_set_id(old.euid, old.suid, old.ruid, new_euid))
        {
            return Err(Errno::EPERM);
        }

        pcb.credentials.ruid = new_ruid;
        pcb.credentials.euid = new_euid;
        if euid != NO_CHANGE {
            pcb.credentials.suid = new_euid;
        }
        Ok(0)
    }

    pub fn sys_setregid(&self, rgid: u32, egid: u32) -> SysResult {
        const NO_CHANGE: u32 = u32::MAX;

        let mut pcb = self.task.pcb.lock();
        let old = pcb.credentials;
        let new_rgid = if rgid == NO_CHANGE { old.rgid } else { rgid };
        let new_egid = if egid == NO_CHANGE { old.egid } else { egid };

        if old.egid != 0
            && (!Self::can_set_id(old.egid, old.sgid, old.rgid, new_rgid)
                || !Self::can_set_id(old.egid, old.sgid, old.rgid, new_egid))
        {
            return Err(Errno::EPERM);
        }

        pcb.credentials.rgid = new_rgid;
        pcb.credentials.egid = new_egid;
        if egid != NO_CHANGE {
            pcb.credentials.sgid = new_egid;
        }
        Ok(0)
    }

    pub fn sys_setresuid(&self, ruid: u32, euid: u32, suid: u32) -> SysResult {
        const NO_CHANGE: u32 = u32::MAX;

        let mut pcb = self.task.pcb.lock();
        let old = pcb.credentials;
        let new_ruid = if ruid == NO_CHANGE { old.ruid } else { ruid };
        let new_euid = if euid == NO_CHANGE { old.euid } else { euid };
        let new_suid = if suid == NO_CHANGE { old.suid } else { suid };

        if old.euid != 0
            && (!Self::can_set_id(old.euid, old.suid, old.ruid, new_ruid)
                || !Self::can_set_id(old.euid, old.suid, old.ruid, new_euid)
                || !Self::can_set_id(old.euid, old.suid, old.ruid, new_suid))
        {
            return Err(Errno::EPERM);
        }

        pcb.credentials.ruid = new_ruid;
        pcb.credentials.euid = new_euid;
        pcb.credentials.suid = new_suid;
        Ok(0)
    }

    pub fn sys_setresgid(&self, rgid: u32, egid: u32, sgid: u32) -> SysResult {
        const NO_CHANGE: u32 = u32::MAX;

        let mut pcb = self.task.pcb.lock();
        let old = pcb.credentials;
        let new_rgid = if rgid == NO_CHANGE { old.rgid } else { rgid };
        let new_egid = if egid == NO_CHANGE { old.egid } else { egid };
        let new_sgid = if sgid == NO_CHANGE { old.sgid } else { sgid };

        if old.egid != 0
            && (!Self::can_set_id(old.egid, old.sgid, old.rgid, new_rgid)
                || !Self::can_set_id(old.egid, old.sgid, old.rgid, new_egid)
                || !Self::can_set_id(old.egid, old.sgid, old.rgid, new_sgid))
        {
            return Err(Errno::EPERM);
        }

        pcb.credentials.rgid = new_rgid;
        pcb.credentials.egid = new_egid;
        pcb.credentials.sgid = new_sgid;
        Ok(0)
    }

    pub fn sys_setgroups(&self) -> SysResult {
        Ok(0)
    }

    pub fn sys_fchown(&self, fd: usize, uid: u32, gid: u32) -> SysResult {
        self.task.get_fd(fd).ok_or(Errno::EBADF)?.chown(uid, gid)?;
        Ok(0)
    }

    pub fn sys_chown(&self, path_ptr: UserRef<i8>, uid: u32, gid: u32) -> SysResult {
        let path = self.read_user_cstr_bounded_len(path_ptr, super::MAX_USER_CSTR_LEN)?;
        let file = self.task.fd_open(AT_FDCWD, &path, OpenFlags::RDONLY)?;
        file.chown(uid, gid)?;
        Ok(0)
    }

    fn directory_search_allowed(stat: &Stat, euid: u32, egid: u32) -> bool {
        if euid == 0 {
            return true;
        }

        let mode = stat.mode.bits();
        if euid == stat.uid {
            mode & StatMode::OWNER_EXEC.bits() != 0
        } else if egid == stat.gid {
            mode & StatMode::GROUP_EXEC.bits() != 0
        } else {
            mode & StatMode::OTHER_EXEC.bits() != 0
        }
    }

    fn chown_prefix_start(&self, dir_fd: isize, path: &str) -> Result<PathBuf, Errno> {
        if path.starts_with('/') {
            return Ok(PathBuf::empty());
        }
        if dir_fd == AT_FDCWD {
            return Ok(self.task.pcb.lock().curr_dir.path_buf());
        }

        let base = self.task.get_fd(dir_fd as usize).ok_or(Errno::EBADF)?;
        if base.file_type()? != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        Ok(base.path_buf())
    }

    fn ensure_lchown_path_prefix(
        &self,
        dir_fd: isize,
        path: &str,
        euid: u32,
        egid: u32,
    ) -> Result<(), Errno> {
        let mut prefix = self.chown_prefix_start(dir_fd, path)?;

        let mut components = path
            .split('/')
            .filter(|component| !component.is_empty() && *component != ".")
            .peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }

            prefix = prefix.join(component);
            let directory = File::open(prefix.clone(), OpenFlags::RDONLY)?;
            let mut stat = Stat::default();
            directory.stat(&mut stat)?;
            if FileType::from(StatMode::from_bits_truncate(stat.mode.bits())) != FileType::Directory
            {
                return Err(Errno::ENOTDIR);
            }
            if !Self::directory_search_allowed(&stat, euid, egid) {
                return Err(Errno::EACCES);
            }
        }

        Ok(())
    }

    fn ensure_lchown_allowed(file: &File, euid: u32) -> Result<(), Errno> {
        if euid == 0 {
            return Ok(());
        }

        let mut stat = Stat::default();
        file.stat(&mut stat)?;
        if stat.uid == euid {
            Ok(())
        } else {
            Err(Errno::EPERM)
        }
    }

    pub fn sys_lchown(&self, path_ptr: UserRef<i8>, uid: u32, gid: u32) -> SysResult {
        let path = self.read_user_cstr_bounded_len(path_ptr, super::MAX_USER_CSTR_LEN)?;
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }

        let credentials = self.task.pcb.lock().credentials;
        self.ensure_lchown_path_prefix(AT_FDCWD, &path, credentials.euid, credentials.egid)?;
        let resolved_path = self.task.fd_resolve(AT_FDCWD, &path)?;
        let file = File::open(resolved_path, OpenFlags::RDONLY)?;
        Self::ensure_lchown_allowed(&file, credentials.euid)?;
        file.chown(uid, gid)?;
        Ok(0)
    }

    pub fn sys_fchownat(
        &self,
        dir_fd: isize,
        path_ptr: UserRef<i8>,
        uid: u32,
        gid: u32,
        flags: u32,
    ) -> SysResult {
        const CHOWNAT_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;

        if flags & !CHOWNAT_FLAGS != 0 {
            return Err(Errno::EINVAL);
        }

        let path = self.read_user_cstr_bounded_len(path_ptr, super::MAX_USER_CSTR_LEN)?;
        if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            if dir_fd == AT_FDCWD {
                self.task.pcb.lock().curr_dir.chown(uid, gid)?;
            } else {
                self.task
                    .get_fd(dir_fd as usize)
                    .ok_or(Errno::EBADF)?
                    .chown(uid, gid)?;
            }
            return Ok(0);
        }

        if path.is_empty() {
            return Err(Errno::ENOENT);
        }

        let credentials = self.task.pcb.lock().credentials;
        self.ensure_lchown_path_prefix(dir_fd, &path, credentials.euid, credentials.egid)?;

        let file = if flags & AT_SYMLINK_NOFOLLOW != 0 {
            let path = self.task.fd_resolve(dir_fd, &path)?;
            File::open(path, OpenFlags::RDONLY)?
        } else {
            self.task.fd_open(dir_fd, &path, OpenFlags::RDONLY)?
        };
        Self::ensure_lchown_allowed(&file, credentials.euid)?;
        file.chown(uid, gid)?;
        Ok(0)
    }

    pub fn sys_chmod(&self, path_ptr: UserRef<i8>, mode: u32) -> SysResult {
        let path = self.read_user_cstr_bounded_len(path_ptr, super::MAX_USER_CSTR_LEN)?;
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }
        let credentials = self.task.pcb.lock().credentials;
        self.ensure_lchown_path_prefix(AT_FDCWD, &path, credentials.euid, credentials.egid)?;
        let resolved_path = self.task.fd_resolve(AT_FDCWD, &path)?;
        let file = File::open_link(resolved_path, OpenFlags::RDONLY)?;
        self.chmod_file(&file, mode)?;
        Ok(0)
    }

    pub fn sys_fchmod(&self, fd: usize, mode: u32) -> SysResult {
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        self.chmod_file(&file, mode)?;
        Ok(0)
    }

    fn chmod_file(&self, file: &File, mode: u32) -> Result<(), Errno> {
        if is_readonly_path(&file.path_buf()) {
            return Err(Errno::EROFS);
        }

        let mut stat = Stat::default();
        file.stat(&mut stat)?;

        let credentials = self.task.pcb.lock().credentials;
        if credentials.euid != 0 && stat.uid != credentials.euid {
            return Err(Errno::EPERM);
        }

        file.chmod(StatMode::from_bits_truncate(mode & MODE_PERMISSIONS))
    }

    pub fn sys_fchmodat(
        &self,
        dir_fd: isize,
        path_ptr: UserRef<i8>,
        mode: u32,
        flags: u32,
    ) -> SysResult {
        if flags & !AT_SYMLINK_NOFOLLOW != 0 {
            return Err(Errno::EINVAL);
        }
        let path = self.read_user_cstr_bounded_len(path_ptr, super::MAX_USER_CSTR_LEN)?;
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }
        if !path.starts_with('/') && dir_fd != AT_FDCWD {
            let base = self.task.get_fd(dir_fd as usize).ok_or(Errno::EBADF)?;
            if base.file_type()? != FileType::Directory {
                return Err(Errno::ENOTDIR);
            }
        }
        let credentials = self.task.pcb.lock().credentials;
        self.ensure_lchown_path_prefix(dir_fd, &path, credentials.euid, credentials.egid)?;
        let file = if flags & AT_SYMLINK_NOFOLLOW != 0 {
            let path = self.task.fd_resolve(dir_fd, &path)?;
            File::open(path, OpenFlags::RDONLY)?
        } else {
            let path = self.task.fd_resolve(dir_fd, &path)?;
            File::open_link(path, OpenFlags::RDONLY)?
        };
        self.chmod_file(&file, mode)?;
        Ok(0)
    }

    pub fn sys_xattr_empty(&self) -> SysResult {
        Err(Errno::ENODATA)
    }

    pub fn sys_xattr_noop(&self) -> SysResult {
        Ok(0)
    }

    pub fn sys_capget(
        &self,
        header: UserRef<UserCapHeader>,
        data: UserRef<UserCapData>,
    ) -> SysResult {
        if header.is_valid() {
            header.with_mut(|header| {
                header.version = 0x2008_0522;
                header.pid = 0;
            });
        }
        if data.is_valid() {
            let data = data.slice_mut_with_len(2);
            data.fill(UserCapData::default());
        }
        Ok(0)
    }

    pub fn sys_capset(&self) -> SysResult {
        Ok(0)
    }

    pub fn sys_personality(&self, _persona: usize) -> SysResult {
        Ok(0)
    }

    pub fn sys_prctl(&self, option: usize, arg2: usize) -> SysResult {
        match option {
            // PR_GET_PDEATHSIG / PR_SET_PDEATHSIG / PR_SET_NAME / PR_GET_NAME /
            // PR_SET_KEEPCAPS / PR_GET_KEEPCAPS are the cheap LTP-visible cases.
            1 if arg2 <= 64 => Ok(0),
            2 => Ok(0),
            15 => Ok(0),
            16 | 8 => Ok(0),
            7 => Ok(0),
            _ => Ok(0),
        }
    }

    pub fn sys_time_adjust_noop(&self, modes_ptr: UserRef<u32>) -> SysResult {
        const ADJ_ADJTIME: u32 = 0x8000;

        let modes = self.read_user_value(modes_ptr)?;
        if modes == ADJ_ADJTIME {
            return Err(Errno::EINVAL);
        }
        Ok(0)
    }

    pub fn sys_umask(&self, _mask: usize) -> SysResult {
        Ok(0)
    }

    pub fn sys_memory_policy_noop(&self) -> SysResult {
        Ok(0)
    }

    pub fn sys_getpgid(&self, pid: usize) -> SysResult {
        let pid = pid as isize;
        if pid < 0 {
            return Err(Errno::ESRCH);
        }

        match pid as usize {
            0 => Ok(0),
            1 => Ok(1),
            pid => tid2task(pid).map(|_| 0).ok_or(Errno::ESRCH),
        }
    }

    pub fn sys_setpgid(&self, _pid: usize, _pgid: usize) -> SysResult {
        Ok(0)
    }

    pub fn sys_klogctl(&self, log_type: usize, buf: UserRef<u8>, len: usize) -> SysResult {
        debug!(
            "sys_klogctl @ log_type: {:?} buf: {:?} len: {:?}",
            log_type, buf, len
        );
        if buf.is_valid() {
            let text = self.read_user_cstr_bounded_len(UserRef::<i8>::from(buf.addr()), len)?;
            println!("{}", text);
        }
        Ok(0)
    }

    pub fn sys_info(&self, meminfo: UserRef<KernelSysInfo>) -> SysResult {
        debug!("sys_info: {}", meminfo);
        if meminfo.is_valid() {
            meminfo.write(KernelSysInfo {
                uptime: 1,
                loads: [0; 3],
                totalram: 1024 * 1024,
                freeram: 512 * 1024,
                sharedram: 0,
                bufferram: 0,
                totalswap: 0,
                freeswap: 0,
                procs: 1,
                pad: 0,
                totalhigh: 0,
                freehigh: 0,
                mem_unit: 1,
                padding: [],
            });
        }
        Ok(0)
    }

    pub fn sys_sched_getparam(&self, pid: usize, param: usize) -> SysResult {
        debug!("sys_sched_getparam @ pid: {} param: {}", pid, param);

        Ok(0)
    }

    pub fn sys_sched_setscheduler(&self, pid: usize, _policy: usize, param: usize) -> SysResult {
        debug!("sys_sched_setscheduler @ pid: {} param: {}", pid, param);

        Ok(0)
    }

    pub fn sys_getrandom(&self, buf: UserRef<u8>, buf_len: usize, flags: usize) -> SysResult {
        debug!(
            "sys_getrandom @ buf: {}, buf_len: {:#x}, flags: {:#x}",
            buf, buf_len, flags
        );
        if flags & !GETRANDOM_FLAGS != 0 {
            return Err(Errno::EINVAL);
        }
        if buf_len == 0 {
            return Ok(0);
        }
        if buf.is_null() {
            return Err(Errno::EFAULT);
        }

        for offset in 0..buf_len {
            let mut seed = GETRANDOM_SEED.load(Ordering::Relaxed);
            loop {
                let next = seed.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(1);
                match GETRANDOM_SEED.compare_exchange_weak(
                    seed,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        self.write_user_bytes(
                            UserRef::from(buf.addr().saturating_add(offset)),
                            &[(next >> 33) as u8],
                        )?;
                        break;
                    }
                    Err(actual) => seed = actual,
                }
            }
        }
        Ok(buf_len)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_arch_prctl(&self, code: usize, addr: usize) -> SysResult {
        use libc_types::others::ArchPrctlCmd;
        use polyhal_trap::trapframe::TrapFrameArgs;
        use syscalls::Errno;

        let arch_prctl_code = ArchPrctlCmd::try_from(code).map_err(|_| Errno::EINVAL)?;
        debug!(
            "sys_arch_prctl @ code: {:?}, addr: {:#x}",
            arch_prctl_code, addr
        );
        let cx_ref = self.task.force_cx_ref();
        match arch_prctl_code {
            ArchPrctlCmd::SetFS => cx_ref[TrapFrameArgs::TLS] = addr,
            _ => {
                error!("arch prctl: {:#x?}", arch_prctl_code);
                return Err(Errno::EPERM);
            }
        }
        Ok(0)
    }
}
