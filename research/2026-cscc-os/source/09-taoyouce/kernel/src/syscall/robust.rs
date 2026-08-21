use super::user::UserSlice;

/// set_robust_list(99) — 存储 robust futex list head
pub(in crate::syscall) fn sys_set_robust_list(head: usize, len: usize) -> isize {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        if let Some(pid) = t.current {
            let (old_head, as_pid) = {
                if let Some(p) = t.get(pid) {
                    let asid = if p.mm.is_some() { pid } else { p.tgid };
                    (p.robust_list_head, asid)
                } else { return -22; }
            };

            // If clearing the list (detached thread exiting), walk old list now
            if old_head != 0 && head == 0 {
                use crate::mm::VirtAddr;
                const FUTEX_OWNER_DIED: u32 = 0x40000000;
                const FUTEX_TID_MASK: u32 = 0x3fffffff;
                const ROBUST_LIST_LIMIT: usize = 2048;
                let tid_val = pid as u32;

                // 未建映射的合法懒页（fork 懒建子 PTE / 懒分配）先走缺页补页再重试。
                let translate_or_fault = |addr_space: &mut crate::mm::AddressSpace,
                                          va: usize,
                                          write: bool|
                 -> Option<crate::mm::PhysPageNum> {
                    let vpn = VirtAddr(va).floor();
                    match addr_space.page_table().translate(vpn) {
                        Some(ppn) if !write => Some(ppn),
                        _ => {
                            if addr_space.handle_page_fault(VirtAddr(va), write) {
                                addr_space.page_table().translate(vpn)
                            } else {
                                addr_space.page_table().translate(vpn)
                            }
                        }
                    }
                };
                let read_usize_va = |va: usize| -> Option<usize> {
                    let target = t.get(as_pid)?;
                    let mut addr_space = target.address_space_lock()?;
                    let off = VirtAddr(va).page_offset();
                    let ppn = translate_or_fault(&mut addr_space, va, false)?;
                    let kva = super::ppn_offset_to_kernel_ptr(ppn, off);
                    Some(unsafe { *(kva as *const usize) })
                };
                let read_i64_va = |va: usize| -> Option<i64> {
                    let target = t.get(as_pid)?;
                    let mut addr_space = target.address_space_lock()?;
                    let off = VirtAddr(va).page_offset();
                    let ppn = translate_or_fault(&mut addr_space, va, false)?;
                    let kva = super::ppn_offset_to_kernel_ptr(ppn, off);
                    Some(unsafe { *(kva as *const i64) })
                };
                let read_u32_va = |va: usize| -> Option<u32> {
                    let target = t.get(as_pid)?;
                    let mut addr_space = target.address_space_lock()?;
                    let off = VirtAddr(va).page_offset();
                    let ppn = translate_or_fault(&mut addr_space, va, false)?;
                    let kva = super::ppn_offset_to_kernel_ptr(ppn, off);
                    Some(unsafe { *(kva as *const u32) })
                };
                let write_u32_va = |va: usize, v: u32| -> Option<()> {
                    let target = t.get(as_pid)?;
                    let mut addr_space = target.address_space_lock()?;
                    let off = VirtAddr(va).page_offset();
                    let ppn = translate_or_fault(&mut addr_space, va, true)?;
                    let kva = super::ppn_offset_to_kernel_ptr(ppn, off);
                    unsafe { *(kva as *mut u32) = v; }
                    Some(())
                };

                let head_next = read_usize_va(old_head).unwrap_or(0);
                let futex_offset = read_i64_va(old_head + 8).unwrap_or(0);
                let pending = read_usize_va(old_head + 16).unwrap_or(0);
                let mut futex_addrs = alloc::vec::Vec::new();

                if pending != 0 {
                    let fa = (pending as i64 + futex_offset) as usize;
                    if let Some(word) = read_u32_va(fa) {
                        if (word & FUTEX_TID_MASK) == tid_val {
                            let _ = write_u32_va(fa, (word & !FUTEX_TID_MASK) | FUTEX_OWNER_DIED);
                            futex_addrs.push(fa);
                        }
                    }
                }
                let mut entry = head_next;
                let mut count = 0usize;
                while entry != old_head && entry != 0 && count < ROBUST_LIST_LIMIT {
                    count += 1;
                    if entry != pending {
                        let fa = (entry as i64 + futex_offset) as usize;
                        if let Some(word) = read_u32_va(fa) {
                            if (word & FUTEX_TID_MASK) == tid_val {
                                let _ = write_u32_va(fa, (word & !FUTEX_TID_MASK) | FUTEX_OWNER_DIED);
                                futex_addrs.push(fa);
                            }
                        }
                    }
                    entry = read_usize_va(entry).unwrap_or(0);
                }

                let pids_to_wake: alloc::vec::Vec<usize> = {
                    let mut waiters = super::FUTEX_WAITERS.lock();
                    let mut pids = alloc::vec::Vec::new();
                    for addr in &futex_addrs {
                        if let Some(queue) = waiters.get(addr) {
                            if let Some(entry) = queue.dequeue_one() {
                                pids.push(entry.pid);
                            }
                            if queue.is_empty() { waiters.remove(addr); }
                        }
                    }
                    pids
                };
                for wpid in pids_to_wake {
                    let should_wake = t.get(wpid).map_or(false, |p| {
                        p.state == crate::sched::process::ProcessState::Sleeping
                    });
                    if should_wake {
                        t.mark_runnable(wpid, true);
                    }
                }
            }

            if let Some(p) = t.get_mut(pid) {
                p.robust_list_head = head;
                p.robust_list_len = len;
                return 0;
            }
        }
    }
    -22
}

/// get_robust_list(100) — 获取 robust futex list head
pub(in crate::syscall) fn sys_get_robust_list(pid_arg: usize, head_ptr: usize, len_ptr: usize) -> isize {
    const CAP_SYS_PTRACE: u64 = 1u64 << 19;
    // 先在锁内取值，释放锁后再写用户空间（avoid deadlock: write_user_buffer also locks PROCESS_TABLE）
    let (head_val, len_val) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_ref() {
            let current_pid = match t.current { Some(p) => p, None => return -3 };
            let target_pid = if pid_arg == 0 { current_pid } else { pid_arg };
            // Permission check: accessing another process requires ptrace-like permission
            if target_pid != current_pid {
                let (my_euid, my_caps) = t.get(current_pid)
                    .map(|p| (p.euid, p.cap_effective))
                    .unwrap_or((u32::MAX, 0));
                if my_euid != 0 && (my_caps & CAP_SYS_PTRACE) == 0 {
                    let target_uid = t.get(target_pid).map(|p| p.euid).unwrap_or(u32::MAX);
                    if my_euid != target_uid {
                        return -1; // -EPERM
                    }
                }
            }
            if let Some(p) = t.get(target_pid) {
                (p.robust_list_head, p.robust_list_len)
            } else {
                return -3; // -ESRCH
            }
        } else {
            return -3;
        }
    };
    // 写回 head 指针
    let head_bytes = (head_val as u64).to_le_bytes();
    if UserSlice::<u8>::new_mut(head_ptr as *mut u8, head_bytes.len()).write_bytes(&head_bytes).is_err() {
        return -14;
    }
    // 写回 len
    let len_bytes = (len_val as u64).to_le_bytes();
    if UserSlice::<u8>::new_mut(len_ptr as *mut u8, len_bytes.len()).write_bytes(&len_bytes).is_err() {
        return -14;
    }
    0
}
