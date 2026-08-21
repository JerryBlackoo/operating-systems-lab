//! Filesystem notification subsystem — inotify, fanotify, dnotify.
//!
//! Mirrors Linux fs/notify/ directory structure.

pub mod inotify;
pub mod fanotify;
pub mod dnotify;
