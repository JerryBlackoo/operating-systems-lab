//! Linux-aligned security subsystem modules.

pub(crate) use crate::syscall::{security, user};

pub mod keyring;
pub mod landlock;
