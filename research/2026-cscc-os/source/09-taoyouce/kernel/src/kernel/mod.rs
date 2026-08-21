//! Linux-aligned core kernel subsystem modules.

pub(crate) use crate::syscall::user;

pub(crate) mod capability;
pub(crate) mod cgroup;
pub(crate) mod coredump;
pub(crate) mod futex;
pub(crate) mod namespace;
pub(crate) mod perf_event;
pub(crate) mod pidfd;
pub mod ptrace;
pub(crate) mod security;
pub(crate) mod signal;
pub(crate) mod signal_syscall;
pub(crate) mod userfaultfd;
