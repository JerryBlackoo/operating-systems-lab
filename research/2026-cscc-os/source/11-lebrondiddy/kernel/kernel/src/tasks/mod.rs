mod async_ops;
pub mod elf;
pub mod exec;
mod filetable;
mod initproc;
mod memset;
mod shm;
mod stack;
mod task;

use self::initproc::initproc;
use crate::{syscall::NET_SERVER, user::entry::user_entry};
use alloc::{
    string::String,
    sync::Weak,
    {sync::Arc, vec::Vec},
};
pub use async_ops::{
    futex_requeue, futex_wake, WaitFutex, WaitHandleAbleSignal, WaitPid, WaitSignal,
};
use devices::get_net_device;
use exec::exec_with_process;
use executor::{
    current_task, set_current_task, thread, yield_now, AsyncTask, TaskId, DEFAULT_EXECUTOR,
};
use fs::pathbuf::PathBuf;
pub use memset::{MapTrack, MemArea, MemType};
use polyhal::common::get_cpu_num;
pub use shm::{MapedSharedMemory, SharedMemory, SHARED_MEMORY};
pub use task::UserTask;

pub enum UserTaskControlFlow {
    Continue,
    Break,
}

#[allow(dead_code)]
pub async fn handle_net() {
    let mut buffer = vec![0u8; 2048];
    // #[cfg(feature = "net")]
    loop {
        let res = get_net_device(0).recv(&mut buffer);
        if let Ok(rlen) = res {
            NET_SERVER.analysis_net_data(&buffer[..rlen]);
        }
        yield_now().await;
    }
}

pub fn init() {
    DEFAULT_EXECUTOR.init(get_cpu_num());
    thread::spawn_blank(initproc());
    // #[cfg(feature = "net")]
    // thread::spawn_blank(KernelTask::new(handle_net()));
}

pub fn run_tasks() {
    DEFAULT_EXECUTOR.run()
}

pub async fn add_user_task_with_cwd(
    work_dir: PathBuf,
    filename: &str,
    args: Vec<&str>,
    envp: Vec<&str>,
) -> TaskId {
    let curr_task = current_task();
    let task = UserTask::new(Weak::new(), work_dir.clone());
    set_current_task(task.clone());
    task.before_run();
    let exec_result = exec_with_process(
        task.clone(),
        work_dir,
        String::from(filename),
        args.into_iter().map(String::from).collect(),
        envp.into_iter().map(String::from).collect(),
    );
    set_current_task(curr_task.clone());
    curr_task.before_run();
    exec_result.expect("can't add task to excutor");
    thread::spawn(task.clone(), user_entry());

    task.get_task_id()
}

#[inline]
pub fn current_user_task() -> Arc<UserTask> {
    current_task().downcast_arc::<UserTask>().ok().unwrap()
}
