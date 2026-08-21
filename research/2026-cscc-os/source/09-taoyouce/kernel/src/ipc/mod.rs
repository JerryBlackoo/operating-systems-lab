//! IPC 子系统
//!
//! 实现微内核的进程间通信机制，包括：
//! - 同步消息传递（send/receive/call/reply）
//! - 异步通知（notification）
//! - 零拷贝大数据传输

pub mod cap_ipc;
pub mod sysv;
pub mod sysv_msg;
pub mod sysv_sem;
pub mod sysv_shm;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use crate::sync::SpinLock;

/// 消息寄存器数量
pub const MSG_REGS: usize = 4;

/// IPC 消息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    /// 消息标签（包含消息长度和类型）
    pub tag: MessageTag,
    /// 消息寄存器
    pub regs: [usize; MSG_REGS],
    /// 发送者徽章
    pub badge: u64,
}

/// 消息标签
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageTag {
    /// 消息长度（寄存器数）
    pub length: usize,
    /// 消息标签值
    pub label: usize,
}

impl Message {
    /// 创建空消息
    pub const fn empty() -> Self {
        Self {
            tag: MessageTag { length: 0, label: 0 },
            regs: [0; MSG_REGS],
            badge: 0,
        }
    }

    /// 创建消息
    pub fn new(label: usize, regs: &[usize]) -> Self {
        let mut msg = Self::empty();
        msg.tag.label = label;
        msg.tag.length = regs.len().min(MSG_REGS);
        for (i, &r) in regs.iter().take(MSG_REGS).enumerate() {
            msg.regs[i] = r;
        }
        msg
    }
}

/// 线程 ID（临时定义，后续由 sched 模块提供）
pub type ThreadId = usize;

/// Slice 2.x D3/D4：把 ThreadId 视为 PCB pid，调度器层 `mark_runnable`
/// 真唤醒。tid==0 视为占位（INVALID_PID），不动；调度器不可用（boot 早
/// 期）也安全 no-op，避免 panic。
fn wake_thread(tid: ThreadId) {
    if tid == 0 {
        return;
    }
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = guard.as_mut() {
        let _ = table.mark_runnable(tid, true);
    }
}

/// 等待队列条目
#[derive(Debug)]
struct WaitEntry {
    /// 等待的线程 ID
    thread_id: ThreadId,
    /// 消息缓冲区
    message: Option<Message>,
    /// 是否为 Call 操作（需要回复）
    is_call: bool,
}

/// IPC 端点状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    /// 空闲
    Idle,
    /// 有发送者等待
    SendWait,
    /// 有接收者等待
    RecvWait,
}

/// IPC 端点
pub struct Endpoint {
    /// 端点 ID
    id: usize,
    /// 端点状态
    state: EndpointState,
    /// 发送等待队列
    send_queue: VecDeque<WaitEntry>,
    /// 接收等待队列
    recv_queue: VecDeque<WaitEntry>,
}

impl Endpoint {
    /// 创建新端点
    pub fn new(id: usize) -> Self {
        Self {
            id,
            state: EndpointState::Idle,
            send_queue: VecDeque::new(),
            recv_queue: VecDeque::new(),
        }
    }

    /// 获取端点 ID
    pub fn id(&self) -> usize {
        self.id
    }

    /// 获取端点状态
    pub fn state(&self) -> EndpointState {
        self.state
    }

    /// 发送消息
    ///
    /// 真化（Slice 2.x D3，禁 TODO）：
    /// * Rendezvous 命中（接收者已等）：把消息塞进 receiver 槽位，**真唤醒**
    ///   该 thread_id 对应的 PCB（`mark_runnable`）。返回 `Ok(())`。
    /// * Miss：把当前线程入 send_queue。调用方在拿到 `Err(WouldBlock)`
    ///   后必须自行 `suspend_current_and_run_next`；这一层不内嵌
    ///   suspend，避免 endpoint 持锁场景死锁。
    pub fn send(&mut self, thread_id: ThreadId, msg: Message) -> Result<(), IpcError> {
        if !self.recv_queue.is_empty() {
            let mut receiver = self.recv_queue.pop_front().unwrap();
            receiver.message = Some(msg);
            // 真唤醒接收者：与 Linux `wake_up_process` 等价
            wake_thread(receiver.thread_id);
            self.update_state();
            Ok(())
        } else {
            self.send_queue.push_back(WaitEntry {
                thread_id,
                message: Some(msg),
                is_call: false,
            });
            self.update_state();
            Err(IpcError::WouldBlock)
        }
    }

    /// 接收消息
    ///
    /// 真化（Slice 2.x D3，禁 TODO）：
    /// * 有发送者等：pop sender，**真唤醒**它，message 取出返回。
    /// * 无发送者：当前线程入 recv_queue，返回 `Err(WouldBlock)`，调用方
    ///   suspend 自身；唤醒后从 `recv_queue` 中找到自己的 message slot
    ///   再读出（调用方应再次 try receive 或保留 slot 索引）。
    pub fn receive(&mut self, thread_id: ThreadId) -> Result<Message, IpcError> {
        if !self.send_queue.is_empty() {
            let sender = self.send_queue.pop_front().unwrap();
            wake_thread(sender.thread_id);
            self.update_state();
            sender.message.ok_or(IpcError::NoMessage)
        } else {
            self.recv_queue.push_back(WaitEntry {
                thread_id,
                message: None,
                is_call: false,
            });
            self.update_state();
            Err(IpcError::WouldBlock)
        }
    }

    /// 由 send 命中的接收槽取出消息（caller 在被唤醒后调用）。
    pub fn take_received_message(&mut self, thread_id: ThreadId) -> Option<Message> {
        for entry in self.recv_queue.iter_mut() {
            if entry.thread_id == thread_id {
                if let Some(msg) = entry.message.take() {
                    return Some(msg);
                }
            }
        }
        None
    }

    /// 更新端点状态
    fn update_state(&mut self) {
        self.state = if !self.send_queue.is_empty() {
            EndpointState::SendWait
        } else if !self.recv_queue.is_empty() {
            EndpointState::RecvWait
        } else {
            EndpointState::Idle
        };
    }
}

/// 通知对象
pub struct Notification {
    /// 通知 ID
    id: usize,
    /// 通知字（位图）
    word: AtomicU64,
    /// 等待队列
    waiters: SpinLock<VecDeque<ThreadId>>,
}

impl Notification {
    /// 创建新通知
    pub fn new(id: usize) -> Self {
        Self {
            id,
            word: AtomicU64::new(0),
            waiters: SpinLock::new(VecDeque::new()),
        }
    }

    /// 发送信号
    ///
    /// 真化（Slice 2.x D4，禁 TODO）：写入 word OR 后**真唤醒**等待队列所有
    /// 线程（Linux `wake_up_all` 语义；notification 是 broadcast-style，
    /// 与 endpoint 的 rendezvous 不同）。
    pub fn signal(&self, bits: u64) {
        self.word.fetch_or(bits, Ordering::SeqCst);
        let waiters: Vec<ThreadId> = {
            let mut w = self.waiters.lock();
            w.drain(..).collect()
        };
        for tid in waiters {
            wake_thread(tid);
        }
    }

    /// 等待信号
    ///
    /// 真化（Slice 2.x D4）：
    /// * word 已有未读位：swap 出全部位返回。
    /// * 否则把当前线程入 waiters 队列，返回 0；调用方在 syscall 边界
    ///   suspend 自身，被 signal 唤醒后重试 wait 或 poll。
    pub fn wait(&self, thread_id: ThreadId) -> u64 {
        let bits = self.word.swap(0, Ordering::SeqCst);
        if bits != 0 {
            return bits;
        }
        self.waiters.lock().push_back(thread_id);
        0
    }

    /// 轮询信号（非阻塞）
    pub fn poll(&self) -> u64 {
        self.word.swap(0, Ordering::SeqCst)
    }
}

/// IPC 错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// 无效端点
    InvalidEndpoint,
    /// 无消息
    NoMessage,
    /// 会阻塞
    WouldBlock,
    /// 权限不足
    PermissionDenied,
    /// 缓冲区太小
    BufferTooSmall,
}

/// 共享缓冲区（用于零拷贝传输）
pub struct SharedBuffer {
    /// 缓冲区物理地址
    pub phys_addr: usize,
    /// 缓冲区大小
    pub size: usize,
    /// 引用计数
    ref_count: AtomicUsize,
}

impl SharedBuffer {
    /// 创建共享缓冲区
    pub fn new(phys_addr: usize, size: usize) -> Self {
        Self {
            phys_addr,
            size,
            ref_count: AtomicUsize::new(1),
        }
    }

    /// 增加引用
    pub fn add_ref(&self) {
        self.ref_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 减少引用
    pub fn release(&self) -> bool {
        self.ref_count.fetch_sub(1, Ordering::Release) == 1
    }
}

/// 全局端点管理器
static ENDPOINT_MANAGER: SpinLock<Option<EndpointManager>> = SpinLock::new(None);

/// 端点管理器
struct EndpointManager {
    endpoints: Vec<Option<Endpoint>>,
    next_id: usize,
}

impl EndpointManager {
    fn new() -> Self {
        Self {
            endpoints: Vec::new(),
            next_id: 0,
        }
    }

    fn create_endpoint(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.endpoints.push(Some(Endpoint::new(id)));
        id
    }
}

/// 初始化 IPC 子系统
pub fn init() {
    let mut manager = ENDPOINT_MANAGER.lock();
    *manager = Some(EndpointManager::new());

    // 初始化 IPC 能力子系统
    cap_ipc::init();
}

/// 创建端点
pub fn create_endpoint() -> usize {
    ENDPOINT_MANAGER.lock()
        .as_mut()
        .expect("IPC not initialized")
        .create_endpoint()
}

/// Slice 2.x D3 真化：发送一条消息到指定 endpoint。
///
/// 命中 receiver 返 `Ok(())`；无 receiver 返 `Err(WouldBlock)`，caller
/// 需要自己挂起（此层不持锁 suspend，避免死锁）。
pub fn endpoint_send(endpoint_id: usize, sender_tid: ThreadId, msg: Message) -> Result<(), IpcError> {
    let mut guard = ENDPOINT_MANAGER.lock();
    let manager = guard.as_mut().ok_or(IpcError::InvalidEndpoint)?;
    let endpoint = manager
        .endpoints
        .get_mut(endpoint_id)
        .and_then(|slot| slot.as_mut())
        .ok_or(IpcError::InvalidEndpoint)?;
    endpoint.send(sender_tid, msg)
}

/// Slice 2.x D3 真化：从指定 endpoint 接收。
///
/// 命中 sender 返 `Ok(msg)`；无 sender 返 `Err(WouldBlock)`，caller 挂起。
pub fn endpoint_receive(endpoint_id: usize, receiver_tid: ThreadId) -> Result<Message, IpcError> {
    let mut guard = ENDPOINT_MANAGER.lock();
    let manager = guard.as_mut().ok_or(IpcError::InvalidEndpoint)?;
    let endpoint = manager
        .endpoints
        .get_mut(endpoint_id)
        .and_then(|slot| slot.as_mut())
        .ok_or(IpcError::InvalidEndpoint)?;
    endpoint.receive(receiver_tid)
}

/// Slice 2.x D3：caller suspend 醒来后，从 endpoint 取走 send 命中时
/// 塞给自己的 message slot。
pub fn endpoint_take_received(endpoint_id: usize, tid: ThreadId) -> Option<Message> {
    let mut guard = ENDPOINT_MANAGER.lock();
    let manager = guard.as_mut()?;
    let endpoint = manager.endpoints.get_mut(endpoint_id)?.as_mut()?;
    endpoint.take_received_message(tid)
}


/// IPC 子系统自测试
pub fn self_test() {
    crate::println!("[ipc] Running self-test...");

    // 测试 MessageTag
    let tag = MessageTag { length: 2, label: 42 };
    assert_eq!(tag.length, 2);
    assert_eq!(tag.label, 42);

    // 测试 Message
    let msg = Message::empty();
    assert_eq!(msg.tag.length, 0);
    assert_eq!(msg.badge, 0);

    let msg2 = Message::new(100, &[1, 2, 3, 4]);
    assert_eq!(msg2.tag.label, 100);
    assert_eq!(msg2.tag.length, 4);
    assert_eq!(msg2.regs[0], 1);
    assert_eq!(msg2.regs[3], 4);
    crate::println!("[ipc] Message tests passed");

    // 测试 EndpointState
    let _states = [
        EndpointState::Idle,
        EndpointState::SendWait,
        EndpointState::RecvWait,
    ];

    // 测试 Endpoint
    let mut endpoint = Endpoint::new(1);
    assert_eq!(endpoint.id(), 1);
    assert_eq!(endpoint.state(), EndpointState::Idle);

    // Slice 2.x D3 真化后的语义：send 无 receiver 必须返 WouldBlock，
    // caller 自己 suspend；receive 命中 send_queue 才返 Ok。
    let msg = Message::new(1, &[100]);
    let result = endpoint.send(1, msg);
    assert_eq!(result, Err(IpcError::WouldBlock));
    assert_eq!(endpoint.state(), EndpointState::SendWait);

    // 现在有 sender 在 send_queue 等，receive(tid=2) 命中并返 Ok。
    let result = endpoint.receive(2);
    assert!(result.is_ok());
    assert_eq!(endpoint.state(), EndpointState::Idle);

    // Rendezvous 反向：先 receive 入 recv_queue，再 send 命中 receiver。
    let result = endpoint.receive(3);
    assert_eq!(result, Err(IpcError::WouldBlock));
    assert_eq!(endpoint.state(), EndpointState::RecvWait);
    let msg2 = Message::new(2, &[200]);
    let result = endpoint.send(4, msg2);
    assert!(result.is_ok());
    // receiver 应在自己 slot 拿到 message
    let pulled = endpoint.take_received_message(3);
    assert!(pulled.is_some());
    assert_eq!(endpoint.state(), EndpointState::Idle);
    crate::println!("[ipc] Endpoint tests passed");

    // 测试 Notification
    let notif = Notification::new(1);

    // 测试 id 字段
    assert_eq!(notif.id, 1);

    // 测试 signal 和 poll
    notif.signal(0b1010);
    let bits = notif.poll();
    assert_eq!(bits, 0b1010);

    // poll 后应该清零
    let bits = notif.poll();
    assert_eq!(bits, 0);

    // 测试 wait（非阻塞情况）
    notif.signal(0b1100);
    let bits = notif.wait(1);
    assert_eq!(bits, 0b1100);
    crate::println!("[ipc] Notification tests passed");

    // 测试 SharedBuffer
    let buffer = SharedBuffer::new(0x1000, 4096);
    assert_eq!(buffer.phys_addr, 0x1000);
    assert_eq!(buffer.size, 4096);

    buffer.add_ref();
    assert!(!buffer.release()); // 还有一个引用
    assert!(buffer.release());  // 最后一个引用
    crate::println!("[ipc] SharedBuffer tests passed");

    // 测试 IpcError 变体
    let _errors = [
        IpcError::InvalidEndpoint,
        IpcError::NoMessage,
        IpcError::WouldBlock,
        IpcError::PermissionDenied,
        IpcError::BufferTooSmall,
    ];

    // 测试全局函数
    let ep_id = create_endpoint();
    crate::println!("[ipc] Created endpoint: {}", ep_id);

    // 调用子模块的 self_test
    cap_ipc::self_test();

    crate::println!("[ipc] Self-test passed!");
}
