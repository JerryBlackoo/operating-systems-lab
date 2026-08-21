use super::SysResult;
use crate::{tasks::WaitHandleAbleSignal, user::UserTaskContainer, utils::useref::UserRef};
use core::{
    future::Future,
    ops::Add,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use executor::select;
use libc_types::{
    time::ITimerVal,
    times::TMS,
    types::{TimeSpec, TimeVal},
};
use log::{debug, warn};
use polyhal::{
    timer::{current_time, get_ticks},
    MappingFlags, VirtAddr,
};
use sync::Mutex;
use syscalls::Errno;

const NANOS_PER_SEC: i128 = 1_000_000_000;

static REALTIME_OFFSET_NS: Mutex<i128> = Mutex::new(0);

fn duration_to_ns(duration: Duration) -> i128 {
    (duration.as_secs() as i128)
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(duration.subsec_nanos() as i128)
}

fn duration_from_ns(ns: i128) -> Duration {
    if ns <= 0 {
        return Duration::ZERO;
    }

    Duration::new((ns / NANOS_PER_SEC) as u64, (ns % NANOS_PER_SEC) as u32)
}

fn realtime_now() -> Duration {
    let offset = *REALTIME_OFFSET_NS.lock();
    duration_from_ns(duration_to_ns(current_time()).saturating_add(offset))
}

fn timespec_to_ns(times: TimeSpec) -> Result<i128, Errno> {
    if times.nsec >= NANOS_PER_SEC as usize || times.sec > i64::MAX as usize {
        return Err(Errno::EINVAL);
    }

    Ok((times.sec as i128)
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(times.nsec as i128))
}

impl UserTaskContainer {
    fn check_user_writable_span(&self, ptr_addr: usize, len: usize) -> Result<(), Errno> {
        if ptr_addr == 0 {
            return Err(Errno::EFAULT);
        }
        for offset in 0..len {
            let Some((_, flags)) = self
                .task
                .page_table
                .translate(VirtAddr::from(ptr_addr.saturating_add(offset)))
            else {
                return Err(Errno::EFAULT);
            };
            super::ensure_user_mapping(flags)?;
            if !flags.contains(MappingFlags::W) {
                return Err(Errno::EFAULT);
            }
        }
        Ok(())
    }

    pub fn sys_gettimeofday(&self, tv_ptr: UserRef<TimeVal>, timezone_ptr: usize) -> SysResult {
        debug!(
            "sys_gettimeofday @ tv_ptr: {}, timezone: {:#x}",
            tv_ptr, timezone_ptr
        );
        if tv_ptr.is_valid() {
            self.write_user_value(tv_ptr, &realtime_now().into())?;
        }
        if timezone_ptr != 0 {
            self.check_user_writable_span(timezone_ptr, core::mem::size_of::<i32>() * 2)?;
        }
        Ok(0)
    }

    pub async fn sys_nanosleep(
        &self,
        req_ptr: UserRef<TimeSpec>,
        rem_ptr: UserRef<TimeSpec>,
    ) -> SysResult {
        debug!(
            "[task {}] sys_nanosleep @ req_ptr: {}, rem_ptr: {}",
            self.tid, req_ptr, rem_ptr
        );
        let req: Duration = req_ptr.read().into();
        debug!("nano sleep {} nseconds", req.as_nanos());

        let res = match select(
            WaitHandleAbleSignal(self.task.clone()),
            WaitUntilsec(current_time() + req),
        )
        .await
        {
            executor::Either::Right(_) => Ok(0),
            executor::Either::Left(_) => {
                if self.task.exit_code().is_some() {
                    Ok(0)
                } else {
                    Err(Errno::EINTR)
                }
            }
        };
        if rem_ptr.is_valid() {
            rem_ptr.write(Default::default());
        }
        res
    }

    pub fn sys_times(&self, tms_ptr: UserRef<TMS>) -> SysResult {
        debug!("sys_times @ tms: {}", tms_ptr);
        self.task.inner_map(|x| tms_ptr.write(x.tms));
        Ok(get_ticks() as _)
    }

    pub fn sys_clock_gettime(&self, clock_id: usize, times_ptr: UserRef<TimeSpec>) -> SysResult {
        debug!(
            "[task {}] sys_clock_gettime @ clock_id: {}, times_ptr: {}",
            self.tid, clock_id, times_ptr
        );

        let dura = match clock_id {
            0 | 5 => realtime_now(),         // CLOCK_REALTIME / CLOCK_REALTIME_COARSE
            1 | 4 | 6 | 7 => current_time(), // MONOTONIC / RAW / COARSE / BOOTTIME
            2 => {
                warn!("CLOCK_PROCESS_CPUTIME_ID not implemented");
                Duration::ZERO
            }
            3 => {
                warn!("CLOCK_THREAD_CPUTIME_ID not implemented");
                Duration::ZERO
            }
            _ => return Err(Errno::EINVAL),
        };
        log::debug!("dura: {:#x?}", dura);
        times_ptr.write(dura.into());
        Ok(0)
    }

    pub fn sys_clock_settime(&self, clock_id: usize, times_ptr: UserRef<TimeSpec>) -> SysResult {
        debug!(
            "[task {}] sys_clock_settime @ clock_id: {}, times_ptr: {}",
            self.tid, clock_id, times_ptr
        );

        if clock_id != 0 {
            return Err(Errno::EINVAL);
        }

        let target = timespec_to_ns(self.read_user_value(times_ptr)?)?;
        let now = duration_to_ns(current_time());
        *REALTIME_OFFSET_NS.lock() = target.saturating_sub(now);
        Ok(0)
    }

    #[inline]
    pub fn sys_clock_getres(&self, clock_id: usize, times_ptr: UserRef<TimeSpec>) -> SysResult {
        debug!("clock_getres @ {} {:#x?}", clock_id, times_ptr);
        if times_ptr.is_valid() {
            times_ptr.write(Duration::from_nanos(1).into());
        }
        Ok(0)
    }
    pub fn sys_setitimer(
        &self,
        which: usize,
        times_ptr: UserRef<ITimerVal>,
        old_timer_ptr: UserRef<ITimerVal>,
    ) -> SysResult {
        debug!(
            "[task {}] sys_setitimer @ which: {} times_ptr: {} old_timer_ptr: {}",
            self.tid, which, times_ptr, old_timer_ptr
        );

        if which == 0 {
            let mut pcb = self.task.pcb.lock();
            if old_timer_ptr.is_valid() {
                old_timer_ptr.write(pcb.timer[0].timer);
            }

            if times_ptr.is_valid() {
                let current_timval: TimeVal = current_time().into();
                let new_timer = times_ptr.read();
                pcb.timer[0].timer = new_timer;
                pcb.timer[0].next = current_timval.add(pcb.timer[0].timer.value);
                if new_timer.value.sec == 0 && new_timer.value.usec == 0 {
                    pcb.timer[0].next = Default::default();
                    pcb.timer[0].last = Default::default();
                }
            }
            Ok(0)
        } else {
            Err(Errno::EPERM)
        }
    }

    pub async fn sys_clock_nanosleep(
        &self,
        clock_id: usize,
        flags: usize,
        req_ptr: UserRef<TimeSpec>,
        rem_ptr: UserRef<TimeSpec>,
    ) -> SysResult {
        debug!(
            "[task {}] sys_clock_nanosleep @ clock_id: {}, flags: {:#x} req_ptr: {}, rem_ptr: {}",
            self.tid, clock_id, flags, req_ptr, rem_ptr
        );

        let interval = req_ptr.read().into();
        if flags == 1 {
            select(
                WaitHandleAbleSignal(self.task.clone()),
                WaitUntilsec(interval),
            )
            .await;
            if rem_ptr.is_valid() {
                rem_ptr.write(Default::default());
            }
        } else {
            debug!("nano sleep {} nseconds", interval.as_nanos());
            select(
                WaitHandleAbleSignal(self.task.clone()),
                WaitUntilsec(current_time() + interval),
            )
            .await;
        }

        Ok(0)
    }
}

pub struct WaitUntilsec(pub Duration);

impl Future for WaitUntilsec {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if crate::tasks::current_user_task().is_killed() {
            return Poll::Ready(());
        }
        let ns = current_time();

        match ns > self.0 {
            true => Poll::Ready(()),
            false => Poll::Pending,
        }
    }
}

#[allow(dead_code)]
pub fn wait_ms(ms: usize) -> WaitUntilsec {
    WaitUntilsec(current_time() + Duration::from_millis(ms as _))
}
