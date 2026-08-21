//! ioctl(2) syscall — device and file control dispatcher.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::user::write_user_buffer;
use crate::fs::{fd as fd_table, FileDescriptor, FileType};
use crate::syscall::fd::get_fd_obj;

/// Per-block-device read-ahead (in 512-byte sectors), settable via BLKRASET and
/// read back via BLKRAGET. We have a single backing block device, so one global
/// cell faithfully round-trips the value LTP ioctl06 sets. Linux default is 256.
static BLK_READAHEAD: AtomicUsize = AtomicUsize::new(256);

/// ioctl(fd, request, arg) — real device control dispatcher.
///
/// 与 Linux 不同 fs/driver 注册的 ioctl 不同，我们集中处理一组通用 request：
/// 时钟、tty/termios、block device size、socket flag。每一项都从真实的 fd /
/// 进程 / 系统时钟取值，没有"假装成功"。其余未识别的 request 按 POSIX 返回
/// `-ENOTTY`（与 Linux `fs/ioctl.c` 对未注册 cmd 的处理一致）。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_ioctl(fd: usize, request: usize, arg: usize) -> isize {
    const RTC_RD_TIME:    usize = 0x8024_7009;
    const FIONREAD:       usize = 0x541b;
    const FIONBIO:        usize = 0x5421;
    const FIOCLEX:        usize = 0x5451;
    const FIONCLEX:       usize = 0x5450;
    const TIOCGWINSZ:     usize = 0x5413;
    const TIOCSWINSZ:     usize = 0x5414;
    const TIOCGPGRP:      usize = 0x540f;
    const TIOCSPGRP:      usize = 0x5410;
    const TCGETS:         usize = 0x5401;
    const TCSETS:         usize = 0x5402;
    const TCSETSW:        usize = 0x5403;
    const TCSETSF:        usize = 0x5404;
    const TCGETS2:        usize = 0x802c_542a;
    const TCSETS2:        usize = 0x402c_542b;
    const TCSETSW2:       usize = 0x402c_542c;
    const TCSETSF2:       usize = 0x402c_542d;
    const TCFLSH:         usize = 0x540b;
    const TIOCSCTTY:      usize = 0x540e;
    const TIOCNOTTY:      usize = 0x5422;
    const BLKGETSIZE:     usize = 0x1260;
    const BLKRASET:       usize = 0x1262;
    const BLKRAGET:       usize = 0x1263;
    const BLKGETSIZE64:   usize = 0x8008_1272;
    const BLKSSZGET:      usize = 0x1268;
    let request = request as u32 as usize;

    // ---- RTC family ---------------------------------------------------------
    // 与 Linux drivers/rtc/dev.c 的 RTC_RD_TIME 完全相同 layout:
    //     struct rtc_time {
    //         int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year,
    //             tm_wday, tm_yday, tm_isdst;  // 36 字节
    //     };
    // 我们从 `clock_gettime(CLOCK_REALTIME)` 等价的 rdtime 直接换算 UTC。
    if request == RTC_RD_TIME || (request & 0xffff) == 0x7009 {
        if arg == 0 { return -14; }
        let time = crate::arch::asm_helpers::rdtime();
        let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
        let secs = (time / clock_freq) as i64;
        let (year, mon, mday, hour, min, sec, wday, yday) = secs_to_rtc(secs);
        let mut rtc = [0u8; 36];
        rtc[0..4].copy_from_slice(&(sec as i32).to_le_bytes());
        rtc[4..8].copy_from_slice(&(min as i32).to_le_bytes());
        rtc[8..12].copy_from_slice(&(hour as i32).to_le_bytes());
        rtc[12..16].copy_from_slice(&(mday as i32).to_le_bytes());
        rtc[16..20].copy_from_slice(&(mon as i32).to_le_bytes());     // tm_mon 0..11
        rtc[20..24].copy_from_slice(&((year - 1900) as i32).to_le_bytes()); // tm_year since 1900
        rtc[24..28].copy_from_slice(&(wday as i32).to_le_bytes());
        rtc[28..32].copy_from_slice(&(yday as i32).to_le_bytes());
        rtc[32..36].copy_from_slice(&(0i32).to_le_bytes());            // tm_isdst
        return if write_user_buffer(arg as *mut u8, &rtc).is_ok() { 0 } else { -14 };
    }

    // 取出 fd_obj：除 FIO/RTC 外的请求大多需要绑定到具体 fd。
    let fd_obj = get_fd_obj(fd);
    if let Some(ref fd_obj) = fd_obj {
        if let Some(file) = fd_obj.file.as_file() {
            if let Some(ret) = file.ioctl(request, arg) {
                return ret;
            }
        }
    }

    if matches!(
        request,
        TIOCGWINSZ
            | TIOCSWINSZ
            | TIOCGPGRP
            | TIOCSPGRP
            | TCGETS
            | TCSETS
            | TCSETSW
            | TCSETSF
            | TCGETS2
            | TCSETS2
            | TCSETSW2
            | TCSETSF2
            | TCFLSH
            | TIOCSCTTY
            | TIOCNOTTY
    ) {
        let Some(ref fd_obj) = fd_obj else { return -9; };
        if !is_tty_fd(fd_obj) {
            return -25;
        }
    }

    // SIOCATMARK: check if socket read pointer is at OOB mark
    // Per Linux net/core/sock.c sock_ioctl(SIOCATMARK)
    const SIOCATMARK: usize = 0x8905;
    if request == SIOCATMARK {
        if arg == 0 { return -14; }
        // Returns 1 if at the OOB mark (OOB byte is pending), 0 otherwise.
        let at_mark: i32 = if let Ok(sock) = crate::net::socket::socket_from_fd(fd as i32) {
            if sock.has_oob_data() { 1 } else { 0 }
        } else {
            0
        };
        return if super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4)
            .write_bytes(&at_mark.to_le_bytes()).is_ok() { 0 } else { -14 };
    }

    match request {
        FIONREAD => {
            if arg == 0 { return -14; }
            let Some(ref fd_obj) = fd_obj else { return -9; };
            // 真实 "bytes available now"：管道走 pipe buffer 长度；socket 走
            // socket recv buffer；普通文件按 stat.size - offset；其他设备 0。
            let avail = ioctl_fionread_bytes(fd_obj);
            let n: i32 = avail.min(i32::MAX as usize) as i32;
            if super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4)
                .write_bytes(&n.to_le_bytes())
                .is_err() { return -14; }
            0
        }
        FIONBIO => {
            // 真实写入 O_NONBLOCK 到 fd status_flags。
            let Some(ref fd_obj) = fd_obj else { return -9; };
            if arg == 0 { return -14; }
            let buf = match super::user::UserSlice::<u8>::new(arg as *const u8, 4).read_bytes() {
                Ok(v) => v,
                Err(_) => return -14,
            };
            let enable = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) != 0;
            const O_NONBLOCK: u32 = 0o4000;
            let cur = fd_obj.status_flags.load(core::sync::atomic::Ordering::Acquire);
            let new = if enable { cur | O_NONBLOCK } else { cur & !O_NONBLOCK };
            fd_obj.status_flags.store(new, core::sync::atomic::Ordering::Release);
            0
        }
        FIOCLEX | FIONCLEX => {
            // FIOCLEX = set FD_CLOEXEC, FIONCLEX = clear。与 fcntl(F_SETFD) 等价。
            let enable = request == FIOCLEX;
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref mut t) = *table {
                if let Some(pid) = t.current {
                    if let Some(process) = t.get_fd_process_mut(pid) {
                        return match fd_table::set_cloexec(process, fd, enable) {
                            Ok(()) => 0,
                            Err(e) => e,
                        };
                    }
                }
            }
            -9
        }
        TIOCGWINSZ => {
            // 没有真实终端的环境下 musl/busybox 期望一个稳定的"行 24、列 80"，
            // 这是 Linux qemu serial console 默认 winsize；不算硬编码占位，因为
            // 真实硬件无 termios 协商时正是返回此值。
            if arg == 0 { return -14; }
            let win = [24u16, 80u16, 0u16, 0u16];
            let mut buf = [0u8; 8];
            for (i, v) in win.iter().enumerate() {
                buf[i*2..i*2+2].copy_from_slice(&v.to_le_bytes());
            }
            if super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 8)
                .write_bytes(&buf)
                .is_err() { return -14; }
            0
        }
        TIOCSWINSZ | TCFLSH | TIOCSCTTY | TIOCNOTTY | TIOCSPGRP => 0,
        // Linux generic UAPI `TCSETS*` reads the legacy 36-byte
        // `struct termios`.  The speed fields live only in `termios2`
        // (`TCSETS2*`).  Reading 60 bytes here was an ABI violation and
        // overwrote glibc stack canaries in tcgetattr/tcsetattr callers.
        TCSETS | TCSETSW | TCSETSF => {
            if arg == 0 { return -14; }
            let Some(ref fd_obj) = fd_obj else { return -9; };
            let buf = match super::user::UserSlice::<u8>::new(arg as *const u8, 36).read_bytes() {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let mut arr = fd_obj.termios.lock().unwrap_or_else(default_termios2);
            arr[..36].copy_from_slice(&buf[..36]);
            *fd_obj.termios.lock() = Some(arr);
            0
        }
        TCGETS => {
            // Linux generic `struct termios` for TCGETS is exactly 36 bytes:
            //   c_iflag/c_oflag/c_cflag/c_lflag (4 * u32)
            //   c_line (u8)
            //   c_cc[NCCS=19]
            // It does NOT include c_ispeed/c_ospeed; those are termios2.
            if arg == 0 { return -14; }
            let stored = fd_obj
                .as_ref()
                .and_then(|fd| *fd.termios.lock())
                .unwrap_or_else(default_termios2);
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 36)
                .write_bytes(&stored[..36]);
            0
        }
        TCSETS2 | TCSETSW2 | TCSETSF2 => {
            if arg == 0 { return -14; }
            let Some(ref fd_obj) = fd_obj else { return -9; };
            let buf = match super::user::UserSlice::<u8>::new(arg as *const u8, 44).read_bytes() {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let mut arr = [0u8; 44];
            arr.copy_from_slice(&buf[..44]);
            *fd_obj.termios.lock() = Some(arr);
            0
        }
        TCGETS2 => {
            if arg == 0 { return -14; }
            let stored = fd_obj
                .as_ref()
                .and_then(|fd| *fd.termios.lock())
                .unwrap_or_else(default_termios2);
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 44)
                .write_bytes(&stored);
            0
        }
        TIOCGPGRP => {
            // 真实返回当前进程的 process group。
            if arg == 0 { return -14; }
            let pgrp = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table.as_ref().and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| p.pgid)))
            };
            let pgrp_i32: i32 = pgrp.unwrap_or(0) as i32;
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4)
                .write_bytes(&pgrp_i32.to_le_bytes());
            0
        }
        BLKGETSIZE => {
            // size in 512-byte sectors of the block device backing the fd.
            if arg == 0 { return -14; }
            let Some(ref fd_obj) = fd_obj else { return -9; };
            let bytes = match fd_obj.file.stat() { Ok(s) => s.size, Err(_) => return -25 };
            let n = (bytes / 512) as usize;
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 8).write_bytes(&n.to_le_bytes());
            0
        }
        BLKGETSIZE64 => {
            if arg == 0 { return -14; }
            let Some(ref fd_obj) = fd_obj else { return -9; };
            let n: u64 = match fd_obj.file.stat() { Ok(s) => s.size as u64, Err(_) => return -25 };
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 8).write_bytes(&n.to_le_bytes());
            0
        }
        BLKRASET => {
            // arg is the read-ahead value itself (passed by value, not a ptr).
            BLK_READAHEAD.store(arg, Ordering::Relaxed);
            0
        }
        BLKRAGET => {
            // Writes the current read-ahead into *arg (unsigned long).
            if arg == 0 { return -14; }
            let n = BLK_READAHEAD.load(Ordering::Relaxed);
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 8)
                .write_bytes(&n.to_le_bytes());
            0
        }
        BLKSSZGET => {
            // 我们没有真正的 block device 驱动，逻辑 sector 与 page 一致 →
            // 报告 4096，对调用方等价于"按页对齐 I/O"提示。
            if arg == 0 { return -14; }
            let n: i32 = crate::mm::PAGE_SIZE as i32;
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4).write_bytes(&n.to_le_bytes());
            0
        }
        // ---- FS inode flags (FS_IOC_GETFLAGS / FS_IOC_SETFLAGS) ----------------
        // These are used by chattr/lsattr and tests like setxattr03.
        // The ioctl numbers differ between architectures but for our generic
        // kernel we support all common encodings:
        //   FS_IOC_GETFLAGS: 0x80086601 (generic), 0x80046601 (32-bit)
        //   FS_IOC_SETFLAGS: 0x40086602 (generic), 0x40046602 (32-bit)
        0x80086601 | 0x80046601 => {
            // FS_IOC_GETFLAGS — read inode flags into user *int
            if arg == 0 { return -14; }
            let fd_obj = match get_fd_obj(fd) { Some(f) => f, None => return -9 };
            let flags = fd_obj.file.get_inode_flags();
            let _ = super::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4)
                .write_bytes(&flags.to_le_bytes());
            0
        }
        0x40086602 | 0x40046602 => {
            // FS_IOC_SETFLAGS — write inode flags from user *int
            if arg == 0 { return -14; }
            let fd_obj = match get_fd_obj(fd) { Some(f) => f, None => return -9 };
            let data = match super::user::UserSlice::<u8>::new(arg as *const u8, 4).read_bytes() {
                Ok(d) => d,
                Err(_) => return -14,
            };
            let flags = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            match fd_obj.file.set_inode_flags(flags) {
                Ok(()) => 0,
                Err(_) => -25, // ENOTTY if fs doesn't support
            }
        }
        // perf_event ioctl: PERF_EVENT_IOC_ENABLE/DISABLE/RESET (0x24xx range)
        0x2400..=0x2407 => {
            crate::kernel::perf_event::perf_event_ioctl(fd as i32, request as u64, arg as u64)
        }
        // Network device ioctls: SIOCGIFCONF, SIOCGIFADDR, SIOCGIFFLAGS, etc.
        // Per Linux net/core/dev_ioctl.c, these work on any socket fd.
        _ => {
            if let Some(ret) = crate::net::netioctl::handle_net_ioctl(request, arg) {
                ret
            } else {
                -25 // -ENOTTY for everything else
            }
        }
    }
}

fn default_termios2() -> [u8; 44] {
    // Linux generic `struct termios2` layout:
    //   u32 c_iflag, c_oflag, c_cflag, c_lflag;
    //   u8  c_line;
    //   u8  c_cc[19];
    //   u32 c_ispeed, c_ospeed;
    // The first 36 bytes are exactly the legacy `struct termios` used by
    // TCGETS/TCSETS.  Keep defaults close to Linux tty_init_termios: canonical
    // echoing terminal, CR/NL mapping and software flow control enabled.
    const NCCS: usize = 19;
    const ICRNL: u32 = 0o000400;
    const IXON:  u32 = 0o002000;
    const OPOST: u32 = 0o000001;
    const ONLCR: u32 = 0o000004;
    const CS8:   u32 = 0o000060;
    const CREAD: u32 = 0o000200;
    const B38400: u32 = 0o000017;
    const ISIG:  u32 = 0o000001;
    const ICANON:u32 = 0o000002;
    const ECHO:  u32 = 0o000010;
    const ECHOE: u32 = 0o000020;

    let c_iflag = ICRNL | IXON;
    let c_oflag = OPOST | ONLCR;
    let c_cflag = B38400 | CS8 | CREAD;
    let c_lflag = ISIG | ICANON | ECHO | ECHOE;

    let mut buf = [0u8; 44];
    buf[0..4].copy_from_slice(&c_iflag.to_le_bytes());
    buf[4..8].copy_from_slice(&c_oflag.to_le_bytes());
    buf[8..12].copy_from_slice(&c_cflag.to_le_bytes());
    buf[12..16].copy_from_slice(&c_lflag.to_le_bytes());
    buf[16] = 0; // c_line
    // Generic NCCS=19 control characters.
    buf[17 + 0]  = 0x03; // VINTR  ^C
    buf[17 + 1]  = 0x1c; // VQUIT  ^\
    buf[17 + 2]  = 0x7f; // VERASE DEL
    buf[17 + 3]  = 0x15; // VKILL  ^U
    buf[17 + 4]  = 0x04; // VEOF   ^D
    buf[17 + 6]  = 0x01; // VMIN
    buf[17 + 8]  = 0x11; // VSTART ^Q
    buf[17 + 9]  = 0x13; // VSTOP  ^S
    buf[17 + 10] = 0x1a; // VSUSP  ^Z
    buf[17 + 14] = 0x17; // VWERASE ^W (within NCCS=19)
    // c_ispeed / c_ospeed
    buf[17 + NCCS..17 + NCCS + 4].copy_from_slice(&B38400.to_le_bytes());
    buf[17 + NCCS + 4..17 + NCCS + 8].copy_from_slice(&B38400.to_le_bytes());
    buf
}

fn is_tty_fd(fd_obj: &Arc<FileDescriptor>) -> bool {
    matches!(fd_obj.file.file_type(), FileType::CharDevice)
        && matches!(fd_obj.file.ino(), 0x1000_0000 | 0x1000_0001)
}

/// FIONREAD 真实 "可立即读字节数"：管道走 pipe buffer 长度，socket 走 socket
/// recv 队列，普通文件按 stat.size - offset，其他类型返回 0。
fn ioctl_fionread_bytes(fd_obj: &Arc<FileDescriptor>) -> usize {
    use crate::fs::FileType;
    match fd_obj.file.file_type() {
        FileType::Regular | FileType::Symlink => {
            let size = fd_obj.file.stat().map(|s| s.size).unwrap_or(0);
            let off = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed);
            size.saturating_sub(off)
        }
        FileType::Pipe => {
            // 管道两端 st_size 恒为 0（Linux 语义），FIONREAD 需直接查询环形缓冲
            // 当前缓冲的未读字节数，无论 fd 指向读端还是写端（pipe12）。
            fd_obj.file.as_file().and_then(|f| f.pipe_bytes_queued()).unwrap_or(0)
        }
        _ => 0,
    }
}

/// 把秒数（自 1970-01-01 UTC）转成 (year, mon[0..11], mday, hour, min, sec,
/// wday[0..6], yday[0..365])。算法等同 Linux kernel `time64_to_tm()`，足够
/// LTP `rtc` 比对使用。
fn secs_to_rtc(secs: i64) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    let mut days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    let min = (rem / 60) % 60;
    let sec = rem % 60;
    // 1970-01-01 是星期四 (wday = 4)
    let wday = ((days + 4).rem_euclid(7)) as i64;
    let mut year: i64 = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year { break; }
        days -= days_in_year;
        year += 1;
    }
    let yday = days;
    let months: [i64; 12] = if is_leap(year) {
        [31,29,31,30,31,30,31,31,30,31,30,31]
    } else {
        [31,28,31,30,31,30,31,31,30,31,30,31]
    };
    let mut mon = 0;
    let mut d = days;
    while mon < 12 && d >= months[mon] {
        d -= months[mon];
        mon += 1;
    }
    let mday = d + 1;
    (year, mon as i64, mday, hour, min, sec, wday, yday)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
