use super::user::UserSlice;

/// getrandom — 使用内核随机数提供者填充
pub(in crate::syscall) fn sys_getrandom(buf: *mut u8, len: usize, _flags: usize) -> isize {
    const GRND_NONBLOCK: usize = 0x0001;
    const GRND_RANDOM: usize = 0x0002;

    if len == 0 {
        return 0;
    }
    if buf.is_null() {
        return -14;
    }
    if _flags & !(GRND_NONBLOCK | GRND_RANDOM) != 0 {
        return -22;
    }
    // Cap to prevent OOM on huge user-provided len (crash02 sends ~2GB)
    let actual_len = len.min(1024 * 1024);
    let mut data = alloc::vec![0u8; actual_len];
    crate::random::fill_bytes(&mut data);
    match UserSlice::<u8>::new_mut(buf, data.len()).write_bytes(&data) {
        Ok(_) => actual_len as isize,
        Err(_) => -14,
    }
}
