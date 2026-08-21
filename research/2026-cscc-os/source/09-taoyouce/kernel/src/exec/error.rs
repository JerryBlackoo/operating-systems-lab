#[derive(Debug)]
#[allow(dead_code)]
pub(super) enum ExecError {
    InvalidElf(crate::loader::LoaderError),
    LoadFailed(crate::loader::LoaderError),
    NoProcess,
    OutOfMemory,
    /// argv+envp exceed the limit that fits on the new process stack (E2BIG).
    ArgListTooLong,
    /// Target file has open-for-write descriptors (ETXTBSY).
    TextBusy,
    /// A fanotify permission monitor denied the exec-time open (EPERM).
    PermissionDenied,
}

pub(super) fn exec_error_to_errno(error: &ExecError) -> isize {
    const ENOENT: isize = -2;
    const EIO: isize = -5;
    const E2BIG: isize = -7;
    const ENOEXEC: isize = -8;
    const ENOMEM: isize = -12;
    const EACCES: isize = -13;
    const EINVAL: isize = -22;

    match error {
        ExecError::InvalidElf(_) => ENOEXEC,
        ExecError::LoadFailed(loader_error) => match loader_error {
            crate::loader::LoaderError::FileNotFound => ENOENT,
            crate::loader::LoaderError::OutOfMemory
            | crate::loader::LoaderError::MappingFailed => ENOMEM,
            crate::loader::LoaderError::IoError => EIO,
            crate::loader::LoaderError::NotAFile => EACCES,
            crate::loader::LoaderError::FileTooSmall
            | crate::loader::LoaderError::InvalidMagic
            | crate::loader::LoaderError::UnsupportedClass
            | crate::loader::LoaderError::UnsupportedEndian
            | crate::loader::LoaderError::UnsupportedVersion
            | crate::loader::LoaderError::UnsupportedArch
            | crate::loader::LoaderError::UnsupportedType
            | crate::loader::LoaderError::InvalidProgramHeaderSize
            | crate::loader::LoaderError::InvalidProgramHeader
            | crate::loader::LoaderError::InvalidSegmentOffset
            | crate::loader::LoaderError::NoLoadableSegments => ENOEXEC,
        },
        ExecError::NoProcess => EINVAL,
        ExecError::OutOfMemory => ENOMEM,
        ExecError::ArgListTooLong => E2BIG,
        ExecError::TextBusy => -26, // ETXTBSY
        ExecError::PermissionDenied => -1, // EPERM
    }
}
