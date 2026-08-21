/// 进程状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// 未使用
    Unused,
    /// 新建（正在初始化）
    Embryo,
    /// 就绪（可运行）
    Runnable,
    /// 运行中
    Running,
    /// 睡眠（等待事件）
    Sleeping,
    Stopped,
    /// 僵尸（已退出，等待父进程回收）
    Zombie,
}

/// 进程退出原因。
#[derive(Debug, Clone, Copy)]
pub enum ExitReason {
    /// 正常退出（exit/_exit/exit_group 携带 status 字节）
    Normal(i32),
    /// 被信号终止。
    Signaled { sig: i32, core_dumped: bool },
}

impl ExitReason {
    /// 信号致死，**未**产生 core dump。
    #[inline]
    pub const fn signaled(sig: i32) -> Self {
        ExitReason::Signaled { sig, core_dumped: false }
    }

    /// 信号致死，**且**产生 core dump。
    #[inline]
    pub const fn signaled_with_dump(sig: i32) -> Self {
        ExitReason::Signaled { sig, core_dumped: true }
    }

    /// SIGKILL（9）便捷构造。
    #[inline]
    pub const fn killed() -> Self {
        ExitReason::Signaled { sig: 9, core_dumped: false }
    }
}

/// 是否为 POSIX 定义的会产 core-dump 的信号（`signal(7)`）。
#[inline]
pub fn signal_is_core_dumping(sig: i32) -> bool {
    matches!(
        sig,
        3   // SIGQUIT
        | 4 // SIGILL
        | 6 // SIGABRT / SIGIOT
        | 7 // SIGBUS
        | 8 // SIGFPE
        | 11 // SIGSEGV
        | 24 // SIGXCPU
        | 25 // SIGXFSZ
        | 31 // SIGSYS
        | 5  // SIGTRAP
    )
}
