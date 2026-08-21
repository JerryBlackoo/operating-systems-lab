#![allow(dead_code)]

pub const SYS_EXIT: usize = 93;
pub const SYS_WRITE: usize = 64;
pub const SYS_READ: usize = 63;
pub const SYS_READV: usize = 65;
pub const SYS_PREAD64: usize = 67;
pub const SYS_PWRITE64: usize = 68;
pub const SYS_FORK: usize = 220;
pub const SYS_EXEC: usize = 221;
pub const SYS_WAITPID: usize = 260;
pub const SYS_WAITID:  usize = 95;
pub const SYS_GETPID: usize = 172;
pub const SYS_YIELD: usize = 124;
pub const SYS_MMAP: usize = 222;
pub const SYS_MUNMAP: usize = 215;
pub const SYS_BRK: usize = 214;

// 文件系统系统调用
pub const SYS_OPEN: usize = 56;
pub const SYS_CLOSE: usize = 57;
pub const SYS_VHANGUP: usize = 58;
pub const SYS_GETCWD: usize = 17;
pub const SYS_CHDIR: usize = 49;
pub const SYS_FCHDIR: usize = 50;
pub const SYS_GETDENTS: usize = 61;
pub const SYS_FSTAT: usize = 80;
pub const SYS_SYNC: usize = 81;
pub const SYS_FSYNC: usize = 82;
pub const SYS_FDATASYNC: usize = 83;
pub const SYS_SYNC_FILE_RANGE: usize = 84;
pub const SYS_SYNCFS: usize = 267;
// 注意: RV/LA 上 #87 实际是 timerfd_gettime（见下方 SYS_TIMERFD_GETTIME）。
// 历史遗留：legacy `unlink` 在 generic uapi 不存在，统一走 unlinkat (#35)。
// 此常量不再被 dispatch — 保留为 1_000_000 的伪 ID 仅供旧代码引用编译，
// 真实 syscall 流量永远走 SYS_UNLINKAT。
pub const SYS_UNLINK: usize = 1_000_000;
pub const SYS_LSEEK: usize = 62;

// 时间相关
pub const SYS_GETTIMEOFDAY: usize = 169;
pub const SYS_CLOCK_GETTIME: usize = 113;
pub const SYS_CLOCK_GETRES: usize = 114;
pub const SYS_ADJTIMEX: usize = 171;

// IO 向量
pub const SYS_WRITEV: usize = 66;
pub const SYS_PREADV: usize = 69;
pub const SYS_PWRITEV: usize = 70;

// 文件预读建议
pub const SYS_FADVISE64: usize = 223;

// 文件控制
pub const SYS_IOCTL: usize = 29;
pub const SYS_FCNTL: usize = 25;
pub const SYS_IOPRIO_SET: usize = 30;
pub const SYS_IOPRIO_GET: usize = 31;

// 管道
pub const SYS_PIPE: usize = 59;

// 系统信息
pub const SYS_SYSINFO: usize = 179;
pub const SYS_ACCT: usize = 89;
pub const SYS_CAPGET: usize = 90;
pub const SYS_CAPSET: usize = 91;
pub const SYS_ADD_KEY: usize = 217;
pub const SYS_REQUEST_KEY: usize = 218;
pub const SYS_KEYCTL: usize = 219;
pub const SYS_BPF: usize = 280;

// musl libc 启动必需
pub const SYS_EXIT_GROUP: usize = 94;
pub const SYS_SET_TID_ADDRESS: usize = 96;
pub const SYS_FUTEX: usize = 98;
pub const SYS_SET_ROBUST_LIST: usize = 99;
pub const SYS_GET_ROBUST_LIST: usize = 100;
pub const SYS_GETITIMER: usize = 102;
pub const SYS_SETITIMER: usize = 103;
pub const SYS_TIMER_CREATE:     usize = 107;
pub const SYS_TIMER_GETTIME:    usize = 108;
pub const SYS_TIMER_GETOVERRUN: usize = 109;
pub const SYS_TIMER_SETTIME:    usize = 110;
pub const SYS_TIMER_DELETE:     usize = 111;
pub const SYS_SIGALTSTACK: usize = 132;
pub const SYS_RT_SIGSUSPEND: usize = 133;
pub const SYS_RT_SIGACTION: usize = 134;
pub const SYS_RT_SIGPROCMASK: usize = 135;
pub const SYS_RT_SIGPENDING: usize = 136;
pub const SYS_RT_SIGTIMEDWAIT: usize = 137;
pub const SYS_MPROTECT: usize = 226;
pub const SYS_RSEQ: usize = 293;

// 进程信息
pub const SYS_GETPPID: usize = 173;
pub const SYS_GETUID: usize = 174;
pub const SYS_GETEUID: usize = 175;
pub const SYS_GETGID: usize = 176;
pub const SYS_GETEGID: usize = 177;
pub const SYS_GETTID: usize = 178;
pub const SYS_SETGID: usize = 144;
pub const SYS_SETUID: usize = 146;
// 注意：seteuid / setegid 在 RV/LA generic uapi 中**不存在独立 syscall**，
// 应通过 setresuid(-1, euid, -1) / setresgid(-1, egid, -1) 实现。
// 这两个旧常量曾被错填为 147 / 149（实际是 setresuid / setresgid），
// 现在改为伪 ID 防止与新的 SYS_SETRESUID/SYS_SETRESGID 冲突。
pub const SYS_SETEUID: usize = 1_000_001;
pub const SYS_SETEGID: usize = 1_000_002;
pub const SYS_GETRESUID: usize = 148;
pub const SYS_GETRESGID: usize = 150;

// 杂项
pub const SYS_UNAME: usize = 160;
pub const SYS_NANOSLEEP: usize = 101;
pub const SYS_PPOLL: usize = 73;
pub const SYS_PSELECT6: usize = 72;

// dup 系列
pub const SYS_DUP: usize = 23;
pub const SYS_DUP3: usize = 24;

// Linux 标准 *at 系列
pub const SYS_MKDIRAT: usize = 34;
pub const SYS_UNLINKAT: usize = 35;
pub const SYS_SYMLINKAT: usize = 36;
pub const SYS_FACCESSAT: usize = 48;
pub const SYS_NEWFSTATAT: usize = 79;
pub const SYS_READLINKAT: usize = 78;
pub const SYS_RENAMEAT:  usize = 38;
pub const SYS_RENAMEAT2: usize = 276;
pub const SYS_PRLIMIT64: usize = 261;
pub const SYS_GETRLIMIT: usize = 163;
pub const SYS_KILL: usize = 129;
pub const SYS_TKILL: usize = 130;
pub const SYS_SIGRETURN: usize = 139;

// IPC 系统调用
pub const SYS_SEND: usize = 300;
pub const SYS_RECEIVE: usize = 301;
pub const SYS_CALL: usize = 302;
pub const SYS_REPLY: usize = 303;

// basic-musl 补全
pub const SYS_TIMES: usize = 153;
pub const SYS_MOUNT: usize = 40;
pub const SYS_UMOUNT2: usize = 39;

// 能力系统调用 (500-506)
pub const SYS_CAP_CREATE: usize = 500;
pub const SYS_CAP_DERIVE: usize = 501;
pub const SYS_CAP_REVOKE: usize = 502;
pub const SYS_CAP_CHECK: usize = 503;
pub const SYS_CAP_INFO: usize = 504;
pub const SYS_CAP_READ: usize = 505;
pub const SYS_CAP_WRITE: usize = 506;

// io_uring 系统调用
pub const SYS_IO_URING_SETUP: usize = 425;
pub const SYS_IO_URING_ENTER: usize = 426;
pub const SYS_IO_URING_REGISTER: usize = 427;

// busybox / musl 补全
pub const SYS_MSYNC: usize = 227;
pub const SYS_MLOCK: usize = 228;
pub const SYS_MUNLOCK: usize = 229;
pub const SYS_MLOCKALL: usize = 230;
pub const SYS_MUNLOCKALL: usize = 231;
pub const SYS_MADVISE: usize = 233;
pub const SYS_GETRANDOM: usize = 278;
pub const SYS_CLOCK_NANOSLEEP: usize = 115;
pub const SYS_WAIT4: usize = 260;
pub const SYS_FCHMODAT: usize = 53;
pub const SYS_FCHOWNAT: usize = 54;
pub const SYS_UTIMENSAT: usize = 88;
pub const SYS_STATX: usize = 291;
pub const SYS_SYSLOG: usize = 116;
pub const SYS_SCHED_SETPARAM: usize = 118;
pub const SYS_SCHED_SETSCHEDULER: usize = 119;
pub const SYS_SCHED_GETSCHEDULER: usize = 120;
pub const SYS_SCHED_GETPARAM: usize = 121;
pub const SYS_SCHED_SETAFFINITY: usize = 122;
pub const SYS_SCHED_GETAFFINITY: usize = 123;
pub const SYS_SCHED_GET_PRIORITY_MAX: usize = 125;
pub const SYS_SCHED_GET_PRIORITY_MIN: usize = 126;
pub const SYS_SCHED_RR_GET_INTERVAL: usize = 127;
pub const SYS_SENDFILE: usize = 71;
pub const SYS_TRUNCATE: usize = 45;
pub const SYS_FTRUNCATE: usize = 46;
pub const SYS_FALLOCATE: usize = 47;

// 事件 FD / 文件监视 FD 系列（最小占位实现，详见 syscall/fd.rs alloc_null_fd）
pub const SYS_EVENTFD2:          usize = 19;
pub const SYS_EPOLL_CREATE1:     usize = 20;
pub const SYS_EPOLL_CTL:         usize = 21;
pub const SYS_EPOLL_PWAIT:       usize = 22;
pub const SYS_INOTIFY_INIT1:     usize = 26;
pub const SYS_INOTIFY_ADD_WATCH: usize = 27;
pub const SYS_INOTIFY_RM_WATCH:  usize = 28;
pub const SYS_SIGNALFD4:         usize = 74;
pub const SYS_TIMERFD_CREATE:    usize = 85;
pub const SYS_TIMERFD_SETTIME:   usize = 86;
pub const SYS_TIMERFD_GETTIME:   usize = 87;

// mknod / link / fanotify / pidfd 系列（最小占位）
pub const SYS_MKNODAT:           usize = 33;
pub const SYS_LINKAT:            usize = 37;
pub const SYS_FANOTIFY_INIT:     usize = 262;
pub const SYS_FANOTIFY_MARK:     usize = 263;
pub const SYS_NAME_TO_HANDLE_AT: usize = 264;
pub const SYS_OPEN_BY_HANDLE_AT: usize = 265;
pub const SYS_PIDFD_OPEN:        usize = 434;
pub const SYS_PIDFD_SEND_SIGNAL: usize = 424;
pub const SYS_PIDFD_GETFD:       usize = 438;
pub const SYS_FCHMOD: usize = 52;
pub const SYS_FCHMODAT2: usize = 452;
pub const SYS_FCHOWN: usize = 55;
pub const SYS_UMASK: usize = 166;
pub const SYS_PRCTL: usize = 167;
pub const SYS_PERSONALITY: usize = 92;
// Slice 2.5：seccomp(2) — POSIX 安全过滤模式入口。Linux 273（generic uapi）。
pub const SYS_SECCOMP: usize = 277;
pub const SYS_SETHOSTNAME: usize = 161;
pub const SYS_SETDOMAINNAME: usize = 162;
pub const SYS_GETRUSAGE: usize = 165;
pub const SYS_TGKILL: usize = 131;
pub const SYS_STATFS: usize = 43;
pub const SYS_FSTATFS: usize = 44;

// 进程组 / 会话
pub const SYS_SETPGID: usize = 154;
pub const SYS_GETPGID: usize = 155;
pub const SYS_SETSID: usize = 157;
pub const SYS_GETGROUPS: usize = 158;
pub const SYS_SETGROUPS: usize = 159;

// socket 族
pub const SYS_SOCKET: usize = 198;

// membarrier
pub const SYS_MEMBARRIER: usize = 283;
pub const SYS_BIND: usize = 200;
pub const SYS_LISTEN: usize = 201;
pub const SYS_ACCEPT: usize = 202;
pub const SYS_ACCEPT4: usize = 242;
pub const SYS_CONNECT: usize = 203;
pub const SYS_GETSOCKNAME: usize = 204;
pub const SYS_GETPEERNAME: usize = 205;
pub const SYS_SENDTO: usize = 206;
pub const SYS_RECVFROM: usize = 207;
pub const SYS_SETSOCKOPT: usize = 208;
pub const SYS_GETSOCKOPT: usize = 209;
pub const SYS_SHUTDOWN: usize = 210;
pub const SYS_SOCKETPAIR: usize = 199;
pub const SYS_GET_MEMPOLICY: usize = 236;

// SysV 共享内存
pub const SYS_SHMGET: usize = 194;
pub const SYS_SHMCTL: usize = 195;
pub const SYS_SHMAT: usize = 196;
pub const SYS_SHMDT: usize = 197;

// SysV 消息队列（stub Ok(0) / -ENOSYS）
pub const SYS_MSGGET: usize = 186;
pub const SYS_MSGCTL: usize = 187;
pub const SYS_MSGRCV: usize = 188;
pub const SYS_MSGSND: usize = 189;

// SysV 信号量（stub Ok(0)）
pub const SYS_SEMGET:      usize = 190;
pub const SYS_SEMCTL:      usize = 191;
pub const SYS_SEMTIMEDOP:  usize = 192;
pub const SYS_SEMOP:       usize = 193;

// POSIX 消息队列（stub -ENOSYS）
pub const SYS_MQ_OPEN:          usize = 180;
pub const SYS_MQ_UNLINK:        usize = 181;
pub const SYS_MQ_TIMEDSEND:     usize = 182;
pub const SYS_MQ_TIMEDRECEIVE:  usize = 183;
pub const SYS_MQ_NOTIFY:        usize = 184;
pub const SYS_MQ_GETSETATTR:    usize = 185;

// 优先级 / 调度策略
pub const SYS_SETPRIORITY: usize = 140;
pub const SYS_GETPRIORITY: usize = 141;

// 配额管理（stub Ok(0)）
pub const SYS_QUOTACTL: usize = 60;

// NUMA 内存迁移（stub Ok(0)）
pub const SYS_MOVE_PAGES:  usize = 239;
pub const SYS_MBIND:       usize = 235;
pub const SYS_SET_MEMPOLICY: usize = 237;
pub const SYS_MIGRATE_PAGES: usize = 238;
pub const SYS_MINCORE:     usize = 232;

// 锁定文件（stub Ok(0)）
pub const SYS_FLOCK: usize = 32;

// 模块管理（stub -EPERM）
pub const SYS_INIT_MODULE:   usize = 105;
pub const SYS_DELETE_MODULE: usize = 106;
pub const SYS_FINIT_MODULE:  usize = 273;

// 命名空间 / chroot 系列（stub）
pub const SYS_CHROOT:      usize = 51;
pub const SYS_PIVOT_ROOT:  usize = 41;
pub const SYS_UNSHARE:     usize = 97;
pub const SYS_SETNS:       usize = 268;

// 扩展属性（stub -ENOTSUP）
pub const SYS_SETXATTR:     usize = 5;
pub const SYS_LSETXATTR:    usize = 6;
pub const SYS_FSETXATTR:    usize = 7;
pub const SYS_GETXATTR:     usize = 8;
pub const SYS_LGETXATTR:    usize = 9;
pub const SYS_FGETXATTR:    usize = 10;
pub const SYS_LISTXATTR:    usize = 11;
pub const SYS_LLISTXATTR:   usize = 12;
pub const SYS_FLISTXATTR:   usize = 13;
pub const SYS_REMOVEXATTR:  usize = 14;
pub const SYS_LREMOVEXATTR: usize = 15;
pub const SYS_FREMOVEXATTR: usize = 16;

// io_setup 系列（stub -ENOSYS）
pub const SYS_IO_SETUP:    usize = 0;
pub const SYS_IO_DESTROY:  usize = 1;
pub const SYS_IO_SUBMIT:   usize = 2;
pub const SYS_IO_CANCEL:   usize = 3;
pub const SYS_IO_GETEVENTS: usize = 4;

// kexec / reboot / swapon 系列（stub -EPERM 拒绝）
pub const SYS_REBOOT:    usize = 142;
pub const SYS_KEXEC_LOAD: usize = 104;
pub const SYS_SWAPON:    usize = 224;
pub const SYS_SWAPOFF:   usize = 225;

// sched_setattr / getattr（stub Ok(0)）
pub const SYS_SCHED_SETATTR: usize = 274;
pub const SYS_SCHED_GETATTR: usize = 275;

// fanotify 之外的扩展（stub）
pub const SYS_PERF_EVENT_OPEN: usize = 241;

// 调试 / ptrace 系列（stub -EPERM 拒绝非 root 跟踪）
pub const SYS_PTRACE:  usize = 117;
pub const SYS_PROCESS_VM_READV:  usize = 270;
pub const SYS_PROCESS_VM_WRITEV: usize = 271;
pub const SYS_KCMP:    usize = 272;

// 信号扩展：rt_sigqueueinfo / rt_tgsigqueueinfo (wrap kill)
pub const SYS_RT_SIGQUEUEINFO:   usize = 138;
pub const SYS_RT_TGSIGQUEUEINFO: usize = 240;

// 进程身份扩展（无真 user/group 表，按 root + 默认 nice 简化）
pub const SYS_GETSID:    usize = 156;
pub const SYS_SETREUID:  usize = 145;
pub const SYS_SETREGID:  usize = 143;
pub const SYS_SETRESUID: usize = 147;
pub const SYS_SETRESGID: usize = 149;
pub const SYS_SETFSUID:  usize = 151;
pub const SYS_SETFSGID:  usize = 152;

// FD 创建系列（null-fd / 真 fd 占位）
pub const SYS_MEMFD_CREATE: usize = 279;
pub const SYS_USERFAULTFD:  usize = 282;
pub const SYS_MEMFD_SECRET: usize = 447;

// 线性扩展 syscall（clone3 / execveat / getcpu）
pub const SYS_CLONE3:    usize = 435;
pub const SYS_EXECVEAT:  usize = 281;
pub const SYS_GETCPU:    usize = 168;

// futex 扩展（futex_waitv = Linux 5.16+；FUTEX_WAKE / WAIT / REQUEUE 是
// SYS_FUTEX 内 op，不是独立 syscall，故只保留 waitv）
pub const SYS_FUTEX_WAITV:  usize = 449;

// 时间扩展
pub const SYS_SETTIMEOFDAY: usize = 170;
pub const SYS_CLOCK_SETTIME: usize = 112;
pub const SYS_CLOCK_ADJTIME: usize = 266;

// landlock (Linux 5.13+) — stub -ENOSYS
pub const SYS_LANDLOCK_CREATE_RULESET: usize = 444;
pub const SYS_LANDLOCK_ADD_RULE:       usize = 445;
pub const SYS_LANDLOCK_RESTRICT_SELF:  usize = 446;

// close_range / openat2 / faccessat2
pub const SYS_OPENAT2:       usize = 437;
pub const SYS_CLOSE_RANGE:   usize = 436;
pub const SYS_FACCESSAT2:    usize = 439;
pub const SYS_PROCESS_MADVISE: usize = 440;

// epoll_pwait2 — 类似 epoll_pwait 但带纳秒精度 timespec
pub const SYS_EPOLL_PWAIT2:  usize = 441;

// 旧名兼容：sys_recvmmsg / sys_sendmmsg
pub const SYS_RECVMMSG: usize = 243;
pub const SYS_SENDMMSG: usize = 269;
pub const SYS_RECVMSG:  usize = 212;
pub const SYS_SENDMSG:  usize = 211;

// mremap / remap_file_pages
pub const SYS_MREMAP:           usize = 216;
pub const SYS_REMAP_FILE_PAGES: usize = 234;

// readahead — purely advisory, return 0
pub const SYS_READAHEAD: usize = 213;

// PKEY mprotect / alloc / free（PKU 相关，stub 0）
pub const SYS_PKEY_MPROTECT: usize = 288;
pub const SYS_PKEY_ALLOC:    usize = 289;
pub const SYS_PKEY_FREE:     usize = 290;

// 高精度 IO（vectored + flags）
pub const SYS_PREADV2:  usize = 286;
pub const SYS_PWRITEV2: usize = 287;
pub const SYS_MLOCK2:   usize = 284;

// pidfd_send_signal 已声明为 424；这里只剩 statmount/listmount (Linux 6.8+)
pub const SYS_STATMOUNT: usize = 457;
pub const SYS_LISTMOUNT: usize = 458;

// 旧名 truncate (RV 没有，跳过；ftruncate 已存在 #46)
// chown / lchown 没有，统一走 fchownat (#54)

// pidfd_send_signal、pidfd_open 已在 mknod 区

// New mount API (Linux 5.2+)
pub const SYS_OPEN_TREE:   usize = 428;
pub const SYS_MOVE_MOUNT:  usize = 429;
pub const SYS_FSOPEN:      usize = 430;
pub const SYS_FSCONFIG:    usize = 431;
pub const SYS_FSMOUNT:     usize = 432;
pub const SYS_FSPICK:      usize = 433;
pub const SYS_MOUNT_SETATTR: usize = 442;

// 决赛线上测例
pub const SYS_VMSPLICE: usize = 75;
pub const SYS_SPLICE: usize = 76;
pub const SYS_TEE: usize = 77;
pub const SYS_COPY_FILE_RANGE: usize = 285;
