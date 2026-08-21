mod fd;
mod mm;
mod shm;
mod signal;
mod socket;
mod sys;
mod task;
mod time;
pub mod types;

use alloc::{string::String, vec::Vec};
use core::mem::{size_of, MaybeUninit};
use libc_types::fcntl::OpenFlags;
#[cfg(target_arch = "x86_64")]
use libc_types::fcntl::AT_FDCWD;
use polyhal::{MappingFlags, VirtAddr};
pub use socket::NET_SERVER;
use sync::Mutex;
use syscalls::{Errno, Sysno};

use log::warn;

use crate::{user::UserTaskContainer, utils::useref::UserRef};

type SysResult = Result<usize, Errno>;

static UNSUPPORTED_SYSCALLS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
const MAX_USER_CSTR_LEN: usize = 4096;

fn ensure_user_mapping(flags: MappingFlags) -> Result<(), Errno> {
    if flags.contains(MappingFlags::U) {
        Ok(())
    } else {
        Err(Errno::EFAULT)
    }
}

fn unsupported_syscall(call_id: usize) -> SysResult {
    let mut logged = UNSUPPORTED_SYSCALLS.lock();
    if logged.iter().all(|seen| *seen != call_id) {
        warn!("unsupported syscall {} -> ENOSYS", call_id);
        logged.push(call_id);
    }
    Err(Errno::ENOSYS)
}

fn unsupported_keyring_syscall() -> SysResult {
    Err(Errno::ENOKEY)
}

impl UserTaskContainer {
    pub(super) fn read_user_value<T: Copy + 'static>(&self, ptr: UserRef<T>) -> Result<T, Errno> {
        if ptr.is_null() {
            return Err(Errno::EFAULT);
        }
        let mut value = MaybeUninit::<T>::uninit();
        let dst = value.as_mut_ptr() as *mut u8;
        for offset in 0..size_of::<T>() {
            if self.task.exit_code().is_some() {
                return Err(Errno::EINTR);
            }
            let vaddr = VirtAddr::from(ptr.addr().saturating_add(offset));
            let Some((paddr, flags)) = self.task.page_table.translate(vaddr) else {
                return Err(Errno::EFAULT);
            };
            ensure_user_mapping(flags)?;
            let byte = unsafe { paddr.get_ptr::<u8>().read() };
            unsafe {
                dst.add(offset).write(byte);
            }
        }
        Ok(unsafe { value.assume_init() })
    }

    pub(super) fn write_user_value<T: 'static>(
        &self,
        ptr: UserRef<T>,
        value: &T,
    ) -> Result<(), Errno> {
        let src = value as *const T as *const u8;
        for offset in 0..size_of::<T>() {
            if self.task.exit_code().is_some() {
                return Err(Errno::EINTR);
            }
            let vaddr = VirtAddr::from(ptr.addr().saturating_add(offset));
            let Some((paddr, flags)) = self.task.page_table.translate(vaddr) else {
                return Err(Errno::EFAULT);
            };
            ensure_user_mapping(flags)?;
            if !flags.contains(MappingFlags::W) {
                return Err(Errno::EFAULT);
            }
            let byte = unsafe { src.add(offset).read() };
            unsafe {
                paddr.get_mut_ptr::<u8>().write(byte);
            }
        }
        Ok(())
    }

    pub(super) fn write_user_bytes(&self, ptr: UserRef<u8>, bytes: &[u8]) -> Result<(), Errno> {
        for (offset, byte) in bytes.iter().enumerate() {
            if self.task.exit_code().is_some() {
                return Err(Errno::EINTR);
            }
            let vaddr = VirtAddr::from(ptr.addr().saturating_add(offset));
            let Some((paddr, flags)) = self.task.page_table.translate(vaddr) else {
                return Err(Errno::EFAULT);
            };
            ensure_user_mapping(flags)?;
            if !flags.contains(MappingFlags::W) {
                return Err(Errno::EFAULT);
            }
            unsafe {
                paddr.get_mut_ptr::<u8>().write(*byte);
            }
        }
        Ok(())
    }

    pub(super) fn read_user_cstr_bounded(&self, ptr: UserRef<i8>) -> Result<String, Errno> {
        self.read_user_cstr_bounded_len(ptr, MAX_USER_CSTR_LEN)
    }

    pub(super) fn read_user_cstr_bounded_len(
        &self,
        ptr: UserRef<i8>,
        max_len: usize,
    ) -> Result<String, Errno> {
        if ptr.is_null() {
            return Err(Errno::EFAULT);
        }
        let mut bytes = Vec::new();
        for offset in 0..max_len {
            if self.task.exit_code().is_some() {
                return Err(Errno::EINTR);
            }
            let vaddr = VirtAddr::from(ptr.addr().saturating_add(offset));
            let Some((paddr, flags)) = self.task.page_table.translate(vaddr) else {
                return Err(Errno::EFAULT);
            };
            ensure_user_mapping(flags)?;
            let byte = unsafe { paddr.get_ptr::<u8>().read() };
            if byte == 0 {
                return String::from_utf8(bytes).map_err(|_| Errno::EINVAL);
            }
            bytes.push(byte);
        }
        Err(Errno::ENAMETOOLONG)
    }

    pub(super) fn read_user_cstr_array_bounded(
        &self,
        ptr: UserRef<UserRef<i8>>,
    ) -> Result<Vec<String>, Errno> {
        const MAX_ENTRIES: usize = 256;
        if ptr.is_null() {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        for index in 0..MAX_ENTRIES {
            if self.task.exit_code().is_some() {
                return Err(Errno::EINTR);
            }
            let entry_addr = ptr
                .addr()
                .saturating_add(index.saturating_mul(core::mem::size_of::<usize>()));
            let mut raw = 0usize;
            for byte_index in 0..core::mem::size_of::<usize>() {
                let vaddr = VirtAddr::from(entry_addr.saturating_add(byte_index));
                let Some((paddr, flags)) = self.task.page_table.translate(vaddr) else {
                    return Err(Errno::EFAULT);
                };
                ensure_user_mapping(flags)?;
                let byte = unsafe { paddr.get_ptr::<u8>().read() };
                raw |= (byte as usize) << (byte_index * 8);
            }
            if raw == 0 {
                return Ok(values);
            }
            values.push(self.read_user_cstr_bounded(UserRef::<i8>::from(raw))?);
        }
        Err(Errno::E2BIG)
    }

    pub async fn syscall(&self, call_id: usize, args: [usize; 6]) -> Result<usize, Errno> {
        let Some(sysno) = Sysno::new(call_id) else {
            return unsupported_syscall(call_id);
        };
        match sysno {
            Sysno::getcwd => self.sys_getcwd(args[0].into(), args[1] as _),
            Sysno::chdir => self.sys_chdir(args[0].into()),
            Sysno::fchdir => self.sys_fchdir(args[0] as _),
            Sysno::chroot => self.sys_chroot(args[0].into()),
            Sysno::openat => {
                self.sys_openat(args[0] as _, args[1].into(), args[2] as _, args[3] as _)
            }
            Sysno::dup => self.sys_dup(args[0]),
            Sysno::dup3 => self.sys_dup3(args[0], args[1], args[2]),
            Sysno::close => self.sys_close(args[0] as _),
            Sysno::close_range => self.sys_close_range(args[0], args[1], args[2]),
            Sysno::mkdirat => {
                self.sys_mkdir_at(args[0] as _, args[1].into(), args[2] as _)
                    .await
            }
            Sysno::mknodat => self.sys_mknodat(args[0] as _, args[1].into(), args[2], args[3]),
            Sysno::fchmod => self.sys_fchmod(args[0] as _, args[1] as _),
            Sysno::fchmodat => self.sys_fchmodat(args[0] as _, args[1].into(), args[2] as _, 0),
            Sysno::fchmodat2 => {
                self.sys_fchmodat(args[0] as _, args[1].into(), args[2] as _, args[3] as _)
            }
            Sysno::read => {
                self.sys_read(args[0] as _, args[1].into(), args[2] as _)
                    .await
            }
            Sysno::write => {
                self.sys_write(args[0] as _, args[1].into(), args[2] as _)
                    .await
            }
            Sysno::execve => {
                self.sys_execve(args[0].into(), args[1].into(), args[2].into())
                    .await
            }
            Sysno::exit => self.sys_exit(args[0] as _),
            Sysno::brk => self.sys_brk(args[0] as _),
            Sysno::getpid => self.sys_getpid().await,
            Sysno::pipe2 => self.sys_pipe2(args[0].into(), args[1] as _),
            Sysno::eventfd2 => self.sys_eventfd2(args[0] as _, args[1] as _),
            Sysno::gettimeofday => self.sys_gettimeofday(args[0].into(), args[1] as _),
            Sysno::nanosleep => self.sys_nanosleep(args[0].into(), args[1].into()).await,
            Sysno::uname => self.sys_uname(args[0].into()),
            Sysno::unlinkat => self.sys_unlinkat(args[0] as _, args[1].into(), args[2] as _),
            Sysno::symlinkat => self.sys_symlinkat(args[0].into(), args[1] as _, args[2].into()),
            Sysno::fstat => self.sys_fstat(args[0] as _, args[1].into()),
            Sysno::wait4 => {
                self.sys_wait4(args[0] as _, args[1].into(), args[2] as _)
                    .await
            }
            Sysno::sched_yield => self.sys_sched_yield().await,
            Sysno::getppid => self.sys_getppid(),
            Sysno::mount => self.sys_mount(
                args[0].into(),
                args[1].into(),
                args[2].into(),
                args[3] as _,
                args[4] as _,
            ),
            Sysno::umount2 => self.sys_umount2(args[0].into(), args[1] as _).await,
            Sysno::mmap => self.sys_mmap(
                args[0] as _,
                args[1] as _,
                args[2] as _,
                args[3] as _,
                args[4] as _,
                args[5] as _,
            ),
            Sysno::munmap => self.sys_munmap(args[0] as _, args[1] as _),
            Sysno::times => self.sys_times(args[0].into()),
            Sysno::getdents64 => self.sys_getdents64(args[0] as _, args[1].into(), args[2] as _),
            Sysno::set_tid_address => self.sys_set_tid_address(args[0] as _),
            Sysno::gettid => self.sys_gettid(),
            Sysno::lseek => self.sys_lseek(args[0] as _, args[1] as _, args[2] as _),
            Sysno::clock_gettime => self.sys_clock_gettime(args[0] as _, args[1].into()),
            Sysno::clock_settime => self.sys_clock_settime(args[0] as _, args[1].into()),
            Sysno::rt_sigtimedwait => self.sys_sigtimedwait().await,
            Sysno::rt_sigsuspend => self.sys_sigsuspend(args[0].into()).await,
            Sysno::prlimit64 => {
                self.sys_prlimit64(args[0] as _, args[1] as _, args[2].into(), args[3].into())
            }
            Sysno::readv => self.sys_readv(args[0] as _, args[1].into(), args[2] as _),
            Sysno::writev => self.sys_writev(args[0] as _, args[1].into(), args[2] as _),
            Sysno::statfs => self.sys_statfs(args[0].into(), args[1].into()),
            Sysno::statx => self.sys_statx(
                args[0] as _,
                args[1].into(),
                args[2] as _,
                args[3] as _,
                args[4].into(),
            ),
            Sysno::pread64 => {
                self.sys_pread(args[0] as _, args[1].into(), args[2] as _, args[3] as _)
            }
            Sysno::pwrite64 => {
                self.sys_pwrite(args[0] as _, args[1].into(), args[2] as _, args[3] as _)
            }
            #[cfg(not(target_arch = "x86_64"))]
            Sysno::fstatat => {
                self.sys_fstatat(args[0] as _, args[1].into(), args[2].into(), args[3] as _)
            }
            #[cfg(target_arch = "x86_64")]
            Sysno::newfstatat => {
                self.sys_fstatat(args[0] as _, args[1].into(), args[2].into(), args[3] as _)
            }
            Sysno::geteuid => self.sys_geteuid(),
            Sysno::getegid => self.sys_getegid(),
            Sysno::getgid => self.sys_getgid(),
            Sysno::getuid => self.sys_getuid(),
            Sysno::getresuid => self.sys_getresuid(args[0].into(), args[1].into(), args[2].into()),
            Sysno::getresgid => self.sys_getresgid(args[0].into(), args[1].into(), args[2].into()),
            Sysno::getgroups => self.sys_getgroups(args[0] as _, args[1].into()),
            Sysno::getpriority => self.sys_getpriority(args[0], args[1]),
            Sysno::capget => self.sys_capget(args[0].into(), args[1].into()),
            Sysno::capset => self.sys_capset(),
            Sysno::setuid => self.sys_setuid(args[0] as _),
            Sysno::setgid => self.sys_setgid(args[0] as _),
            Sysno::setreuid => self.sys_setreuid(args[0] as _, args[1] as _),
            Sysno::setregid => self.sys_setregid(args[0] as _, args[1] as _),
            Sysno::setresuid => self.sys_setresuid(args[0] as _, args[1] as _, args[2] as _),
            Sysno::setresgid => self.sys_setresgid(args[0] as _, args[1] as _, args[2] as _),
            Sysno::setgroups => self.sys_setgroups(),
            Sysno::fchown => self.sys_fchown(args[0] as _, args[1] as _, args[2] as _),
            Sysno::fchownat => self.sys_fchownat(
                args[0] as _,
                args[1].into(),
                args[2] as _,
                args[3] as _,
                args[4] as _,
            ),
            Sysno::setxattr
            | Sysno::lsetxattr
            | Sysno::fsetxattr
            | Sysno::removexattr
            | Sysno::lremovexattr
            | Sysno::fremovexattr => self.sys_xattr_noop(),
            Sysno::getxattr
            | Sysno::lgetxattr
            | Sysno::fgetxattr
            | Sysno::listxattr
            | Sysno::llistxattr
            | Sysno::flistxattr => self.sys_xattr_empty(),
            Sysno::getpgid => self.sys_getpgid(args[0]),
            Sysno::personality => self.sys_personality(args[0]),
            Sysno::ioctl => {
                self.sys_ioctl(
                    args[0] as _,
                    args[1] as _,
                    args[2] as _,
                    args[3] as _,
                    args[4] as _,
                )
                .await
            }
            Sysno::fcntl => self.sys_fcntl(args[0] as _, args[1] as _, args[2] as _),
            Sysno::utimensat => {
                self.sys_utimensat(args[0] as _, args[1].into(), args[2].into(), args[3] as _)
            }
            Sysno::rt_sigprocmask => {
                self.sys_sigprocmask(args[0] as _, args[1].into(), args[2].into())
            }
            Sysno::rt_sigaction => self.sys_sigaction(args[0] as _, args[1].into(), args[2].into()),
            Sysno::mprotect => self.sys_mprotect(args[0] as _, args[1] as _, args[2] as _),
            Sysno::futex => {
                self.sys_futex(
                    args[0].into(),
                    args[1] as _,
                    args[2] as _,
                    args[3] as _,
                    args[4] as _,
                    args[5] as _,
                )
                .await
            }
            Sysno::readlinkat => {
                self.sys_readlinkat(args[0] as _, args[1].into(), args[2].into(), args[3] as _)
            }
            Sysno::sendfile => {
                self.sys_sendfile(args[0] as _, args[1] as _, args[2] as _, args[3] as _)
            }
            Sysno::tkill => self.sys_tkill(args[0] as _, args[1] as _),
            Sysno::rt_sigreturn => self.sys_sigreturn(),
            Sysno::get_robust_list => {
                warn!("SYS_GET_ROBUST_LIST @ ");
                Ok(0)
            } // always ok for now
            Sysno::set_robust_list => Ok(0),
            Sysno::ppoll => {
                self.sys_ppoll(args[0].into(), args[1] as _, args[2].into(), args[3] as _)
                    .await
            }
            Sysno::getrusage => self.sys_getrusage(args[0] as _, args[1].into()),
            Sysno::setpgid => self.sys_setpgid(args[0] as _, args[1] as _),
            Sysno::umask => self.sys_umask(args[0]),
            Sysno::prctl => self.sys_prctl(args[0], args[1]),
            Sysno::adjtimex => self.sys_time_adjust_noop(args[0].into()),
            Sysno::clock_adjtime => self.sys_time_adjust_noop(args[1].into()),
            Sysno::pselect6 => {
                self.sys_pselect(
                    args[0] as _,
                    args[1].into(),
                    args[2].into(),
                    args[3].into(),
                    args[4].into(),
                    args[5] as _,
                )
                .await
            }
            Sysno::kill => self.sys_kill(args[0] as _, args[1] as _).await,
            Sysno::fsync | Sysno::fdatasync => self.sys_fsync(args[0]),
            Sysno::faccessat => self.sys_faccess_at(args[0] as _, args[1].into(), args[2], 0),
            Sysno::faccessat2 => {
                self.sys_faccess_at(args[0] as _, args[1].into(), args[2], args[3])
            }
            Sysno::socket => self.sys_socket(args[0] as _, args[1] as _, args[2] as _),
            Sysno::socketpair => {
                self.sys_socket_pair(args[0] as _, args[1] as _, args[2] as _, args[3].into())
            }
            Sysno::bind => self.sys_bind(args[0] as _, args[1].into(), args[2] as _),
            Sysno::listen => self.sys_listen(args[0] as _, args[1] as _),
            Sysno::accept => {
                self.sys_accept(args[0] as _, args[1] as _, args[2] as _)
                    .await
            }
            Sysno::accept4 => {
                self.sys_accept4(args[0] as _, args[1].into(), args[2] as _, args[3] as _)
                    .await
            }
            Sysno::connect => {
                self.sys_connect(args[0] as _, args[1].into(), args[2] as _)
                    .await
            }
            Sysno::recvfrom => {
                self.sys_recvfrom(
                    args[0] as _,
                    args[1].into(),
                    args[2] as _,
                    args[3] as _,
                    args[4].into(),
                    args[5].into(),
                )
                .await
            }
            Sysno::sendto => {
                self.sys_sendto(
                    args[0] as _,
                    args[1].into(),
                    args[2] as _,
                    args[3] as _,
                    args[4].into(),
                    args[5].into(),
                )
                .await
            }
            Sysno::syslog => self.sys_klogctl(args[0] as _, args[1].into(), args[2] as _),
            Sysno::sysinfo => self.sys_info(args[0].into()),
            Sysno::msync => self.sys_msync(args[0], args[1], args[2] as _),
            Sysno::mlock
            | Sysno::munlock
            | Sysno::mlockall
            | Sysno::munlockall
            | Sysno::mlock2
            | Sysno::mincore => self.sys_memory_policy_noop(),
            Sysno::exit_group => self.sys_exit_group(args[0]),
            Sysno::truncate => self.sys_truncate(args[0].into(), args[1]),
            Sysno::ftruncate => self.sys_ftruncate(args[0], args[1]),
            Sysno::fallocate => self.sys_fallocate(args[0], args[1], args[2], args[3]),
            Sysno::shmget => self.sys_shmget(args[0] as _, args[1] as _, args[2] as _),
            Sysno::shmat => self.sys_shmat(args[0] as _, args[1] as _, args[2] as _),
            Sysno::shmctl => self.sys_shmctl(args[0] as _, args[1] as _, args[2] as _),
            Sysno::setitimer => self.sys_setitimer(args[0] as _, args[1].into(), args[2].into()),
            Sysno::setsockopt => self.sys_setsockopt(
                args[0] as _,
                args[1] as _,
                args[2] as _,
                args[3] as _,
                args[4] as _,
            ),
            Sysno::getsockopt => self.sys_getsockopt(
                args[0] as _,
                args[1] as _,
                args[2] as _,
                args[3].into(),
                args[4].into(),
            ),
            Sysno::getsockname => {
                self.sys_getsockname(args[0] as _, args[1].into(), args[2].into())
            }
            Sysno::getpeername => {
                self.sys_getpeername(args[0] as _, args[1].into(), args[2].into())
            }
            Sysno::getsid => self.sys_getsid(args[0]),
            Sysno::setsid => self.sys_setsid(),
            Sysno::shutdown => self.sys_shutdown(args[0] as _, args[1] as _),
            Sysno::sched_getparam => self.sys_sched_getparam(args[0] as _, args[1] as _),
            Sysno::sched_setscheduler => {
                self.sys_sched_setscheduler(args[0] as _, args[1] as _, args[2] as _)
            }
            Sysno::clock_getres => self.sys_clock_getres(args[0] as _, args[1].into()),
            Sysno::clock_nanosleep => {
                self.sys_clock_nanosleep(args[0] as _, args[1] as _, args[2].into(), args[3].into())
                    .await
            }
            Sysno::epoll_create1 => self.sys_epoll_create1(args[0] as _),
            Sysno::epoll_ctl => {
                self.sys_epoll_ctl(args[0] as _, args[1] as _, args[2] as _, args[3].into())
            }
            Sysno::epoll_pwait => {
                self.sys_epoll_wait(
                    args[0] as _,
                    args[1].into(),
                    args[2] as _,
                    args[3] as _,
                    args[4] as _,
                )
                .await
            }
            Sysno::epoll_pwait2 => {
                self.sys_epoll_pwait2(
                    args[0] as _,
                    args[1].into(),
                    args[2] as _,
                    args[3].into(),
                    args[4] as _,
                )
                .await
            }
            Sysno::copy_file_range => self.sys_copy_file_range(
                args[0] as _,
                args[1].into(),
                args[2] as _,
                args[3].into(),
                args[4],
                args[5] as _,
            ),
            Sysno::getrandom => self.sys_getrandom(args[0].into(), args[1] as _, args[2] as _),
            Sysno::sched_setaffinity => {
                log::debug!("sys_setaffinity() ");
                Ok(0)
            }
            Sysno::sched_getscheduler => {
                log::debug!("sys_sched_getscheduler");
                Ok(0)
            }
            Sysno::sched_getaffinity => {
                self.sys_sched_getaffinity(args[0], args[1], args[2].into())
            }
            Sysno::add_key | Sysno::request_key | Sysno::keyctl => unsupported_keyring_syscall(),
            Sysno::renameat2 => self.sys_renameat2(
                args[0] as _,
                args[1].into(),
                args[2] as _,
                args[3].into(),
                args[4],
            ),
            Sysno::renameat => self.sys_renameat2(
                args[0] as _,
                args[1].into(),
                args[2] as _,
                args[3].into(),
                OpenFlags::RDWR.bits(),
            ),
            #[cfg(not(any(target_arch = "x86_64")))]
            Sysno::clone => {
                self.sys_clone(
                    args[0] as _,
                    args[1] as _,
                    args[2].into(),
                    args[3] as _,
                    args[4].into(),
                )
                .await
            }
            #[cfg(any(target_arch = "x86_64"))]
            Sysno::pause => self.sys_pause().await,
            #[cfg(any(target_arch = "x86_64"))]
            Sysno::clone => {
                self.sys_clone(
                    args[0] as _,
                    args[1] as _,
                    args[2].into(),
                    args[4] as _,
                    args[3].into(),
                )
                .await
            }
            #[cfg(target_arch = "x86_64")]
            Sysno::rename => self.sys_renameat2(
                AT_FDCWD,
                args[0].into(),
                AT_FDCWD,
                args[1].into(),
                OpenFlags::RDWR.bits(),
            ),
            #[cfg(target_arch = "x86_64")]
            Sysno::select => {
                self.sys_select(
                    args[0] as _,
                    args[1].into(),
                    args[2].into(),
                    args[3].into(),
                    args[4].into(),
                )
                .await
            }
            #[cfg(target_arch = "x86_64")]
            Sysno::mkdir => self.sys_mkdir(args[0].into(), args[1]),
            #[cfg(target_arch = "x86_64")]
            Sysno::chmod => self.sys_chmod(args[0].into(), args[1] as _),
            #[cfg(target_arch = "x86_64")]
            Sysno::readlink => self.sys_readlink(args[0].into(), args[1].into(), args[2]),
            #[cfg(target_arch = "x86_64")]
            Sysno::symlink => self.sys_symlinkat(args[0].into(), AT_FDCWD, args[1].into()),
            #[cfg(target_arch = "x86_64")]
            Sysno::arch_prctl => self.sys_arch_prctl(args[0], args[1]),
            #[cfg(target_arch = "x86_64")]
            Sysno::open => self.sys_open(args[0].into(), args[1], args[2]),
            #[cfg(target_arch = "x86_64")]
            Sysno::fork => self.sys_fork().await,
            #[cfg(target_arch = "x86_64")]
            Sysno::pipe => self.sys_pipe2(args[0].into(), 0),
            #[cfg(target_arch = "x86_64")]
            Sysno::unlink => self.sys_unlink(args[0].into()),
            #[cfg(target_arch = "x86_64")]
            Sysno::rmdir => self.sys_rmdir(args[0].into()),
            #[cfg(target_arch = "x86_64")]
            Sysno::poll => self.sys_poll(args[0].into(), args[1], args[2] as _).await,
            #[cfg(target_arch = "x86_64")]
            Sysno::stat => self.sys_stat(args[0].into(), args[1].into()),
            #[cfg(target_arch = "x86_64")]
            Sysno::lstat => self.sys_lstat(args[0].into(), args[1].into()),
            #[cfg(target_arch = "x86_64")]
            Sysno::dup2 => self.sys_dup2(args[0], args[1]),
            #[cfg(target_arch = "x86_64")]
            Sysno::sync | Sysno::access => Ok(0),
            _ => unsupported_syscall(call_id),
        }
    }
}
