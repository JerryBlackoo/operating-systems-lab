use super::{
    filetable::{rlimits_new, FileTable},
    memset::{MemSet, MemType},
    shm::MapedSharedMemory,
};
use crate::{
    syscall::types::time::ProcessTimer,
    tasks::{
        futex_wake,
        memset::{MapTrack, MemArea},
    },
};
use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    cmp::{max, min},
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};
use devices::PAGE_SIZE;
use executor::{release_task, task::TaskType, task_id_alloc, AsyncTask, TaskId};
use fs::{file::File, pathbuf::PathBuf, INodeInterface};
use libc_types::{
    fcntl::{OpenFlags, AT_FDCWD},
    internal::SigAction,
    signal::{SignalNum, REAL_TIME_SIGNAL_NUM},
    times::TMS,
    types::SigSet,
};
use log::{debug, warn};
use polyhal::{va, MappingFlags, MappingSize, PageTableWrapper, PhysAddr, VirtAddr};
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use runtime::frame::{alignup, frame_alloc_much};
use sync::{Mutex, MutexGuard, RwLock};
use syscalls::Errno;
use vfscore::VfsResult;

pub type FutexTable = BTreeMap<usize, Vec<usize>>;

#[derive(Clone, Copy)]
pub struct Credentials {
    pub ruid: u32,
    pub euid: u32,
    pub suid: u32,
    pub rgid: u32,
    pub egid: u32,
    pub sgid: u32,
}

impl Credentials {
    pub const fn root() -> Self {
        Self {
            ruid: 0,
            euid: 0,
            suid: 0,
            rgid: 0,
            egid: 0,
            sgid: 0,
        }
    }
}

pub struct ProcessControlBlock {
    pub memset: MemSet,
    pub fd_table: FileTable,
    pub credentials: Credentials,
    pub root_dir: PathBuf,
    pub curr_dir: File,
    pub exe_path: String,
    pub heap: usize,
    pub entry: usize,
    pub children: Vec<Arc<UserTask>>,
    pub tms: TMS,
    pub rlimits: Vec<usize>,
    pub sigaction: [SigAction; 65],
    pub futex_table: Arc<Mutex<FutexTable>>,
    pub shms: Vec<MapedSharedMemory>,
    pub timer: [ProcessTimer; 3],
    pub threads: Vec<Weak<UserTask>>,
    pub exit_code: Option<usize>,
}

pub struct ThreadControlBlock {
    pub cx: TrapFrame,
    pub sigmask: SigSet,
    pub clear_child_tid: usize,
    pub set_child_tid: usize,
    pub signal: SigSet,
    pub signal_queue: [usize; REAL_TIME_SIGNAL_NUM], // a queue for real time signals
    /// 低 7 位：终止信号编号（如 SIGKILL 为 9）
    /// 第 8 位（bit 7）：是否生成 core dump（core 被生成则为 1）
    /// 高 8 位：如果是正常退出（如 exit(3)），则 exit_code = 3 << 8
    pub exit_signal: u8,
    pub thread_exit_code: Option<u32>,
}

pub struct UserTask {
    pub task_id: TaskId,
    pub process_id: TaskId,
    pub page_table: Arc<PageTableWrapper>,
    pub pcb: Arc<Mutex<ProcessControlBlock>>,
    pub parent: RwLock<Weak<UserTask>>,
    pub tcb: RwLock<ThreadControlBlock>,
    wall_deadline_ns: AtomicU64,
}

impl UserTask {
    fn clear_child_tid(&self, uaddr: usize) {
        if uaddr == 0 {
            return;
        }
        debug!("write addr: {:#x}", uaddr);
        if let Some((addr, _)) = self.page_table.translate(VirtAddr::from(uaddr)) {
            unsafe {
                addr.get_mut_ptr::<u32>().write(0);
            }
            futex_wake(self.pcb.lock().futex_table.clone(), uaddr, 1);
        } else {
            debug!("clear_child_tid address is not mapped: {:#x}", uaddr);
        }
    }

    fn try_clear_child_tid_from_timer(&self, uaddr: usize, futex_table: &Arc<Mutex<FutexTable>>) {
        if uaddr == 0 {
            return;
        }
        if let Some((addr, _)) = self.page_table.translate(VirtAddr::from(uaddr)) {
            unsafe {
                addr.get_mut_ptr::<u32>().write(0);
            }
        }
        if let Some(mut table) = futex_table.try_lock() {
            if let Some(queue) = table.get_mut(&uaddr) {
                let wake_count = min(1, queue.len());
                queue.drain(..wake_count);
            }
        }
    }

    pub fn release(&self) {
        // Ensure that the task was exited successfully.
        assert!(self.final_exit_code().is_some() || self.tcb.read().thread_exit_code.is_some());
        release_task(self.task_id);
    }
}

impl UserTask {
    fn open_initial_cwd(work_dir: &PathBuf) -> File {
        match File::open(work_dir.clone(), OpenFlags::DIRECTORY) {
            Ok(curr_dir) => curr_dir,
            Err(err) => {
                warn!(
                    "task cwd {} unavailable ({:?}); falling back to /",
                    work_dir.path(),
                    err
                );
                File::open("/", OpenFlags::DIRECTORY).expect("root directory must exist")
            }
        }
    }

    pub fn cleanup_resources_after_exit(&self) {
        if self.task_id == self.process_id || Arc::strong_count(&self.pcb) == 1 {
            self.pcb.lock().memset.clear();
            self.pcb.lock().fd_table.clear();
            self.pcb.lock().children.clear();
        }
    }

    fn finish_process_exit(&self, exit_code: usize) {
        let tcb_writer = self.tcb.write();
        let uaddr = tcb_writer.clear_child_tid;
        self.clear_child_tid(uaddr);
        self.pcb.lock().exit_code = Some(exit_code);
        let exit_signal = tcb_writer.exit_signal;
        drop(tcb_writer);

        self.cleanup_resources_after_exit();

        if let Some(parent) = self.parent.read().upgrade() {
            if exit_signal != 0 {
                parent
                    .tcb
                    .write()
                    .signal
                    .insert(SignalNum::try_from(exit_signal).unwrap());
            } else {
                parent.tcb.write().signal.insert(SignalNum::CHLD);
            }
        } else {
            self.pcb.lock().children.clear();
        }
    }

    fn signal_parent_from_timer(&self, exit_signal: u8) {
        let Some(parent_ref) = self.parent.try_read() else {
            return;
        };
        let Some(parent) = parent_ref.upgrade() else {
            return;
        };
        let Some(mut parent_tcb) = parent.tcb.try_write() else {
            return;
        };

        if exit_signal != 0 {
            parent_tcb
                .signal
                .insert(SignalNum::try_from(exit_signal).unwrap());
        } else {
            parent_tcb.signal.insert(SignalNum::CHLD);
        }
    }

    pub fn new(parent: Weak<UserTask>, work_dir: PathBuf) -> Arc<Self> {
        let task_id = task_id_alloc();
        // initialize memset
        let memset = MemSet::new(vec![]);

        let curr_dir = Self::open_initial_cwd(&work_dir);
        let root_dir = PathBuf::empty();
        const SIGACTION: SigAction = SigAction::empty();

        let inner = ProcessControlBlock {
            memset,
            fd_table: FileTable::new(),
            credentials: Credentials::root(),
            root_dir,
            curr_dir,
            exe_path: String::new(),
            heap: 0,
            children: Vec::new(),
            entry: 0,
            tms: Default::default(),
            rlimits: rlimits_new(),
            sigaction: [SIGACTION; 65],
            futex_table: Arc::new(Mutex::new(BTreeMap::new())),
            shms: vec![],
            timer: [Default::default(); 3],
            exit_code: None,
            threads: Vec::new(),
        };

        let tcb = RwLock::new(ThreadControlBlock {
            cx: TrapFrame::new(),
            sigmask: SigSet::empty(),
            clear_child_tid: 0,
            set_child_tid: 0,
            signal: SigSet::empty(),
            signal_queue: [0; REAL_TIME_SIGNAL_NUM],
            exit_signal: 0,
            thread_exit_code: Option::None,
        });

        let task = Arc::new(Self {
            page_table: Arc::new(PageTableWrapper::alloc()),
            task_id,
            process_id: task_id,
            parent: RwLock::new(parent),
            pcb: Arc::new(Mutex::new(inner)),
            tcb,
            wall_deadline_ns: AtomicU64::new(crate::watchdog::new_child_task_deadline_ns(task_id)),
        });
        task.pcb.lock().threads.push(Arc::downgrade(&task));
        crate::watchdog::register_user_task(Arc::downgrade(&task));
        task
    }

    pub fn inner_map<T>(&self, mut f: impl FnMut(&mut MutexGuard<ProcessControlBlock>) -> T) -> T {
        f(&mut self.pcb.lock())
    }

    #[inline]
    pub fn map(&self, paddr: PhysAddr, vaddr: VirtAddr, flags: MappingFlags) {
        assert_eq!(paddr.raw() % PAGE_SIZE, 0);
        assert_eq!(vaddr.raw() % PAGE_SIZE, 0);
        // self.page_table.map(ppn, vpn, flags, 3);
        self.page_table
            .map_page(vaddr, paddr, flags, MappingSize::Page4KB);
    }

    #[inline]
    pub fn frame_alloc(&self, vaddr: VirtAddr, mtype: MemType, count: usize) -> Option<PhysAddr> {
        self.map_frames(vaddr, mtype, count, None, 0, vaddr.raw(), count * PAGE_SIZE)
    }

    pub fn map_frames(
        &self,
        vaddr: VirtAddr,
        mtype: MemType,
        count: usize,
        file: Option<Arc<dyn INodeInterface>>,
        offset: usize,
        start: usize,
        len: usize,
    ) -> Option<PhysAddr> {
        assert!(count > 0, "can't alloc count = 0 in user_task frame_alloc");
        // alloc trackers and map vpn
        let trackers: Vec<_> = frame_alloc_much(count)?
            .into_iter()
            .enumerate()
            .map(|(i, x)| {
                let vaddr = match vaddr.raw() == 0 {
                    true => vaddr,
                    false => va!(vaddr.raw() + i * PAGE_SIZE),
                };
                MapTrack {
                    vaddr,
                    tracker: Arc::new(x),
                    rwx: 0,
                }
            })
            .collect();
        if vaddr.raw() != 0 {
            debug!(
                "map {:?} @ {:#x} size: {:#x} flags: {:?}",
                vaddr,
                trackers[0].tracker.raw(),
                count * PAGE_SIZE,
                MappingFlags::URWX
            );
            // map vpn to ppn
            trackers
                .clone()
                .iter()
                .filter(|x| x.vaddr.raw() != 0)
                .for_each(|x| self.map(x.tracker.0, x.vaddr, MappingFlags::URWX));
        }
        let mut inner = self.pcb.lock();
        let ppn = trackers[0].tracker.0;
        if mtype == MemType::Stack {
            let finded_area = inner.memset.iter_mut().find(|x| x.mtype == mtype);
            if let Some(area) = finded_area {
                area.mtrackers.extend(trackers);
            } else if mtype == MemType::Stack {
                inner.memset.push(MemArea {
                    mtype,
                    mtrackers: trackers.clone(),
                    file: None,
                    offset: 0,
                    start: 0x7000_0000,
                    len: 0x1000_0000,
                });
            }
        } else {
            inner.memset.push(MemArea {
                mtype,
                mtrackers: trackers.clone(),
                file,
                offset,
                start,
                len,
            });
        }
        drop(inner);

        Some(ppn)
    }

    pub fn force_cx_ref(&self) -> &'static mut TrapFrame {
        unsafe { &mut self.tcb.as_mut_ptr().as_mut().unwrap().cx }
    }

    pub fn sbrk(&self, addr: usize) -> usize {
        let curr_page = self.pcb.lock().heap.div_ceil(PAGE_SIZE);
        let after_page = addr.div_ceil(PAGE_SIZE);
        // 如果需要申请内存
        (curr_page..after_page).for_each(|i| {
            self.frame_alloc(va!(i * PAGE_SIZE), MemType::CodeSection, 1);
        });
        self.pcb.lock().heap = addr;
        addr
    }

    #[inline]
    pub fn thread_exit(&self, exit_code: usize) {
        let mut tcb_writer = self.tcb.write();
        let uaddr = tcb_writer.clear_child_tid;
        self.clear_child_tid(uaddr);
        tcb_writer.thread_exit_code = Some(exit_code as u32);
        let exit_signal = tcb_writer.exit_signal;
        drop(tcb_writer);

        if self.task_id == self.process_id || Arc::strong_count(&self.pcb) == 1 {
            self.cleanup_resources_after_exit();
            self.pcb.lock().exit_code = Some(exit_code);

            if let Some(parent) = self.parent.read().upgrade() {
                if exit_signal != 0 {
                    // parent
                    //     .tcb
                    //     .write()
                    //     .signal
                    //     .add_signal(SignalNum::from_num(exit_signal as _));
                    parent
                        .tcb
                        .write()
                        .signal
                        .insert(SignalNum::try_from(exit_signal).unwrap());
                } else {
                    parent.tcb.write().signal.insert(SignalNum::CHLD);
                }
            }
        }

        // If this is not the main thread, Just exit immediately, don't store any resources.
        if self.task_id != self.process_id {
            self.pcb
                .lock()
                .children
                .retain(|x| x.task_id != self.task_id);
            self.release();
        }
    }

    #[inline]
    pub fn exit_with_signal(&self, signal: usize) {
        self.exit(128 + signal);
    }

    #[inline]
    pub fn is_killed(&self) -> bool {
        self.exit_code().is_some()
    }

    #[inline]
    pub fn exit_code(&self) -> Option<usize> {
        self.final_exit_code()
    }

    #[inline]
    pub fn final_exit_code(&self) -> Option<usize> {
        self.pcb.lock().exit_code
    }

    pub fn check_wall_deadline_from_timer(&self) {
        let deadline = self.wall_deadline_ns.load(Ordering::Relaxed);
        if deadline == 0 || crate::watchdog::current_time_ns() < deadline {
            return;
        }
        let Some(mut pcb) = self.pcb.try_lock() else {
            return;
        };
        if pcb.exit_code.is_some() {
            return;
        }
        if self
            .wall_deadline_ns
            .compare_exchange(deadline, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        pcb.exit_code = Some(124);
        let futex_table = pcb.futex_table.clone();
        drop(pcb);

        let mut exit_signal = 0;
        let mut clear_child_tid = 0;
        let mut syscall = None;
        if let Some(mut tcb) = self.tcb.try_write() {
            tcb.thread_exit_code = Some(124);
            exit_signal = tcb.exit_signal;
            clear_child_tid = tcb.clear_child_tid;
            syscall = Some(tcb.cx[TrapFrameArgs::SYSCALL]);
        }

        if let Some(syscall) = syscall {
            println!(
                "kernel watchdog: task {} syscall {} exceeded wall-clock limit; killed with SIGKILL semantics",
                self.task_id,
                syscall
            );
        } else {
            println!(
                "kernel watchdog: task {} exceeded wall-clock limit; killed with SIGKILL semantics",
                self.task_id
            );
        }
        self.try_clear_child_tid_from_timer(clear_child_tid, &futex_table);
        self.signal_parent_from_timer(exit_signal);
    }

    pub fn wall_deadline_fired(&self) -> bool {
        self.wall_deadline_ns.load(Ordering::Relaxed) == 0 && self.final_exit_code().is_some()
    }

    pub fn set_wall_deadline_after(&self, duration: core::time::Duration) {
        self.wall_deadline_ns
            .store(crate::watchdog::deadline_after(duration), Ordering::Relaxed);
    }

    pub fn check_wall_deadline(&self) {
        self.check_wall_deadline_from_timer();
    }

    #[inline]
    pub fn cow_fork(self: Arc<Self>) -> Arc<Self> {
        // Give the frame_tracker in the memset a type.
        // it will contains the frame used for page mapping、
        // mmap or text section.
        // and then we can implement COW(copy on write).
        let parent_task: Arc<UserTask> = self.clone();
        let work_dir = parent_task.clone().pcb.lock().curr_dir.path_buf();
        let new_task = Self::new(Arc::downgrade(&parent_task), work_dir);
        let mut new_tcb_writer = new_task.tcb.write();
        // clone fd_table and clone heap
        let mut new_pcb = new_task.pcb.lock();
        let mut pcb = self.pcb.lock();
        new_pcb.fd_table = pcb.fd_table.clone();
        new_pcb.credentials = pcb.credentials;
        new_pcb.heap = pcb.heap;
        new_tcb_writer.cx = self.tcb.read().cx.clone();
        new_tcb_writer.cx[TrapFrameArgs::RET] = 0;
        new_pcb.root_dir = pcb.root_dir.clone();
        new_pcb.curr_dir = pcb.curr_dir.clone();
        new_pcb.exe_path = pcb.exe_path.clone();
        pcb.children.push(new_task.clone());
        new_pcb.shms = pcb.shms.clone();
        drop(new_pcb);

        // cow fork
        pcb.memset.iter().for_each(|x| {
            let map_area = x.clone();
            let flags = match x.mtype {
                MemType::Shared | MemType::ShareFile => MappingFlags::URWX,
                _ => MappingFlags::URX,
            };
            map_area.mtrackers.iter().for_each(|x| {
                new_task.map(x.tracker.0, x.vaddr, flags);
                self.map(x.tracker.0, x.vaddr, flags);
            });
            new_task.pcb.lock().memset.push(map_area);
        });
        drop(new_tcb_writer);
        // copy shm and map them
        pcb.shms.iter().for_each(|x| {
            x.mem.trackers.iter().enumerate().for_each(|(i, tracker)| {
                new_task.map(tracker.0, va!(x.start + i * PAGE_SIZE), MappingFlags::URWX);
            });
        });
        new_task
    }

    #[inline]
    pub fn thread_clone(self: Arc<Self>) -> Arc<Self> {
        // Give the frame_tracker in the memset a type.
        // it will contains the frame used for page mapping、
        // mmap or text section.
        // and then we can implement COW(copy on write).
        let parent_tcb = self.tcb.read();

        let task_id = task_id_alloc();
        let mut pcb = self.pcb.lock();
        let tcb = RwLock::new(ThreadControlBlock {
            cx: parent_tcb.cx.clone(),
            sigmask: parent_tcb.sigmask.clone(),
            clear_child_tid: 0,
            set_child_tid: 0,
            signal: SigSet::empty(),
            signal_queue: [0; REAL_TIME_SIGNAL_NUM],
            exit_signal: 0,
            thread_exit_code: Option::None,
        });

        tcb.write().cx[TrapFrameArgs::RET] = 0;
        drop(parent_tcb);

        let new_task = Arc::new(Self {
            page_table: self.page_table.clone(),
            task_id,
            process_id: self.task_id,
            parent: RwLock::new(self.parent.read().clone()),
            pcb: self.pcb.clone(),
            tcb,
            wall_deadline_ns: AtomicU64::new(crate::watchdog::new_child_task_deadline_ns(task_id)),
        });
        pcb.threads.push(Arc::downgrade(&new_task));
        crate::watchdog::register_user_task(Arc::downgrade(&new_task));
        new_task
    }

    pub fn push(&self, val: usize) {
        let mut tcb = self.tcb.write();
        let sp = tcb.cx[TrapFrameArgs::SP] - size_of::<usize>();
        *va!(sp).get_mut_ref() = val;
        tcb.cx[TrapFrameArgs::SP] = sp;
    }

    #[inline]
    pub fn push_str(&self, s: &str) -> usize {
        self.push_arr(s.as_bytes())
    }

    pub fn push_arr(&self, buffer: &[u8]) -> usize {
        let mut tcb = self.tcb.write();

        const ULEN: usize = size_of::<usize>();
        let len = buffer.len();
        let sp = tcb.cx[TrapFrameArgs::SP] - alignup(len + 1, ULEN);
        va!(sp).slice_mut_with_len(len).copy_from_slice(buffer);
        tcb.cx[TrapFrameArgs::SP] = sp;
        sp
    }

    pub fn get_last_free_addr(&self) -> VirtAddr {
        let map_last = self
            .pcb
            .lock()
            .memset
            .iter()
            .filter(|x| x.mtype != MemType::Stack)
            .fold(0, |acc, x| max(acc, x.start + x.len));
        let shm_last = self
            .pcb
            .lock()
            .shms
            .iter()
            .fold(0, |acc, v| max(v.start + v.size, acc));
        va!(max(map_last, shm_last))
    }

    pub fn get_fd(&self, index: usize) -> Option<Arc<File>> {
        let pcb = self.pcb.lock();
        (index < pcb.rlimits[7] && index < pcb.fd_table.len())
            .then(|| pcb.fd_table[index].clone())
            .flatten()
    }

    pub fn set_fd(&self, index: usize, value: Arc<File>) {
        let mut pcb = self.pcb.lock();
        if index < pcb.rlimits[7] && index < pcb.fd_table.len() {
            pcb.fd_table[index] = Some(value);
        }
    }

    pub fn clear_fd(&self, index: usize) {
        let mut pcb = self.pcb.lock();
        (index < pcb.fd_table.len()).then(|| pcb.fd_table[index] = None);
    }

    pub fn alloc_fd(&self) -> Option<usize> {
        let mut pcb = self.pcb.lock();
        let index = pcb.fd_table.iter().position(|x| x.is_none());
        if index.is_none() && pcb.fd_table.len() < pcb.rlimits[7] {
            pcb.fd_table.push(None);
            Some(pcb.fd_table.len() - 1)
        } else {
            index
        }
    }

    pub fn alloc_fd_at_or_above(&self, min_fd: usize) -> Option<usize> {
        let mut pcb = self.pcb.lock();
        if min_fd >= pcb.rlimits[7] {
            return None;
        }

        for index in min_fd..pcb.fd_table.len() {
            if pcb.fd_table[index].is_none() {
                return Some(index);
            }
        }

        while pcb.fd_table.len() <= min_fd {
            if pcb.fd_table.len() >= pcb.rlimits[7] {
                return None;
            }
            pcb.fd_table.push(None);
        }
        Some(min_fd)
    }

    pub fn fd_open(&self, fd: isize, filename: &str, flags: OpenFlags) -> VfsResult<File> {
        let path = self.fd_resolve(fd, filename)?;
        File::open(path, flags)
    }

    #[inline]
    pub fn fd_resolve(&self, fd: isize, filename: &str) -> VfsResult<PathBuf> {
        if filename.starts_with("/") {
            let root_dir = self.pcb.lock().root_dir.clone();
            if root_dir.levels() == 0 {
                Ok(filename.into())
            } else {
                Ok(root_dir.join(filename.trim_start_matches('/')))
            }
        } else {
            let parent = match fd {
                AT_FDCWD => self.pcb.lock().curr_dir.clone(),
                _ => self
                    .pcb
                    .lock()
                    .fd_table
                    .get(fd as usize)
                    .cloned()
                    .flatten()
                    .ok_or(Errno::EBADF)?
                    .as_ref()
                    .clone(),
            };
            Ok(parent.path_buf().join(filename))
        }
    }
}

impl AsyncTask for UserTask {
    fn before_run(&self) {
        self.check_wall_deadline();
        self.page_table.change();
    }

    fn get_task_id(&self) -> TaskId {
        self.task_id
    }

    fn get_task_type(&self) -> TaskType {
        TaskType::MonolithicTask
    }

    #[inline]
    fn exit(&self, exit_code: usize) {
        self.finish_process_exit(exit_code);
    }

    #[inline]
    fn exit_code(&self) -> Option<usize> {
        self.pcb.lock().exit_code
    }
}
