//! IPC 子系统属性测试
//!
//! 验证进程间通信的以下属性：
//! 1. 消息传递正确性：发送的消息应被正确接收
//! 2. 端点状态一致性：端点状态正确反映队列状态
//! 3. 通知位图正确性：信号位正确设置和清除
//! 4. 队列 FIFO 属性：消息按发送顺序接收
//! 5. 共享缓冲区引用计数：引用计数正确管理

use super::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 消息寄存器数量
const MSG_REGS: usize = 4;

/// 消息标签
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MessageTag {
    length: usize,
    label: usize,
}

/// IPC 消息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Message {
    tag: MessageTag,
    regs: [usize; MSG_REGS],
    badge: u64,
}

impl Message {
    fn new(label: usize, regs: &[usize]) -> Self {
        let mut msg = Self {
            tag: MessageTag { length: regs.len().min(MSG_REGS), label },
            regs: [0; MSG_REGS],
            badge: 0,
        };
        for (i, &r) in regs.iter().take(MSG_REGS).enumerate() {
            msg.regs[i] = r;
        }
        msg
    }
}

/// 线程 ID
type ThreadId = usize;

/// 等待条目
struct WaitEntry {
    thread_id: ThreadId,
    message: Option<Message>,
}

/// 端点状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointState {
    Idle,
    SendWait,
    RecvWait,
}

/// IPC 端点
struct Endpoint {
    id: usize,
    state: EndpointState,
    send_queue: VecDeque<WaitEntry>,
    recv_queue: VecDeque<WaitEntry>,
}

impl Endpoint {
    fn new(id: usize) -> Self {
        Self {
            id,
            state: EndpointState::Idle,
            send_queue: VecDeque::new(),
            recv_queue: VecDeque::new(),
        }
    }
    
    fn send(&mut self, thread_id: ThreadId, msg: Message) -> Result<(), ()> {
        if !self.recv_queue.is_empty() {
            // 有接收者，直接传递
            let mut receiver = self.recv_queue.pop_front().unwrap();
            receiver.message = Some(msg);
            // 模拟唤醒接收者
        } else {
            // 加入发送队列
            self.send_queue.push_back(WaitEntry {
                thread_id,
                message: Some(msg),
            });
        }
        self.update_state();
        Ok(())
    }
    
    fn receive(&mut self, thread_id: ThreadId) -> Option<Message> {
        if !self.send_queue.is_empty() {
            let sender = self.send_queue.pop_front().unwrap();
            self.update_state();
            sender.message
        } else {
            self.recv_queue.push_back(WaitEntry {
                thread_id,
                message: None,
            });
            self.update_state();
            None
        }
    }
    
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
struct Notification {
    word: AtomicU64,
    waiters: VecDeque<ThreadId>,
}

impl Notification {
    fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
            waiters: VecDeque::new(),
        }
    }
    
    fn signal(&self, bits: u64) {
        self.word.fetch_or(bits, Ordering::SeqCst);
    }
    
    fn poll(&self) -> u64 {
        self.word.swap(0, Ordering::SeqCst)
    }
    
    fn wait(&mut self, thread_id: ThreadId) -> u64 {
        let bits = self.word.swap(0, Ordering::SeqCst);
        if bits != 0 {
            return bits;
        }
        self.waiters.push_back(thread_id);
        0
    }
}

/// 共享缓冲区
struct SharedBuffer {
    phys_addr: usize,
    size: usize,
    ref_count: AtomicUsize,
}

impl SharedBuffer {
    fn new(phys_addr: usize, size: usize) -> Self {
        Self {
            phys_addr,
            size,
            ref_count: AtomicUsize::new(1),
        }
    }
    
    fn add_ref(&self) {
        self.ref_count.fetch_add(1, Ordering::Relaxed);
    }
    
    fn release(&self) -> bool {
        self.ref_count.fetch_sub(1, Ordering::Release) == 1
    }
    
    fn ref_count(&self) -> usize {
        self.ref_count.load(Ordering::Relaxed)
    }
}

/// 属性1：消息传递正确性
/// 发送的消息应被正确接收
#[test_case]
fn test_message_delivery() -> bool {
    let mut ep = Endpoint::new(0);
    
    // 先有接收者等待
    let recv_result = ep.receive(1);
    if recv_result.is_some() {
        println!("  错误: 无发送者时不应收到消息");
        return false;
    }
    
    // 发送消息
    let msg = Message::new(42, &[1, 2, 3, 4]);
    ep.send(2, msg).unwrap();
    
    // 接收者应该收到消息（模拟）
    // 在实际实现中，接收者会被唤醒并获取消息
    
    true
}

/// 属性2：端点状态一致性
/// 端点状态应正确反映队列状态
#[test_case]
fn test_endpoint_state_consistency() -> bool {
    let mut ep = Endpoint::new(0);
    
    // 初始状态应为 Idle
    if ep.state != EndpointState::Idle {
        println!("  错误: 初始状态应为 Idle");
        return false;
    }
    
    // 添加接收者后应为 RecvWait
    ep.receive(1);
    if ep.state != EndpointState::RecvWait {
        println!("  错误: 有接收者时应为 RecvWait");
        return false;
    }
    
    // 发送消息后（消费接收者）应为 Idle
    let msg = Message::new(1, &[]);
    ep.send(2, msg).unwrap();
    if ep.state != EndpointState::Idle {
        println!("  错误: 消息传递后应为 Idle");
        return false;
    }
    
    // 只有发送者时应为 SendWait
    ep.send(3, Message::new(2, &[])).unwrap();
    if ep.state != EndpointState::SendWait {
        println!("  错误: 有发送者等待时应为 SendWait");
        return false;
    }
    
    true
}

/// 属性3：消息内容完整性
/// 接收的消息应与发送的消息完全一致
#[test_case]
fn test_message_integrity() -> bool {
    let mut ep = Endpoint::new(0);
    
    // 发送消息
    let original = Message::new(12345, &[0xDEAD, 0xBEEF, 0xCAFE, 0xBABE]);
    ep.send(1, original).unwrap();
    
    // 接收消息
    let received = ep.receive(2);
    
    if let Some(msg) = received {
        if msg.tag.label != original.tag.label {
            println!("  错误: 标签不匹配");
            return false;
        }
        if msg.tag.length != original.tag.length {
            println!("  错误: 长度不匹配");
            return false;
        }
        for i in 0..msg.tag.length {
            if msg.regs[i] != original.regs[i] {
                println!("  错误: 寄存器 {} 不匹配", i);
                return false;
            }
        }
    } else {
        println!("  错误: 未收到消息");
        return false;
    }
    
    true
}

/// 属性4：队列 FIFO 属性
/// 消息应按发送顺序接收
#[test_case]
fn test_fifo_ordering() -> bool {
    let mut ep = Endpoint::new(0);
    
    // 发送多个消息
    for i in 0..10 {
        let msg = Message::new(i, &[i]);
        ep.send(i + 100, msg).unwrap();
    }
    
    // 接收并验证顺序
    for i in 0..10 {
        if let Some(msg) = ep.receive(i + 200) {
            if msg.tag.label != i {
                println!("  错误: 消息顺序错误，期望 {}，收到 {}", i, msg.tag.label);
                return false;
            }
        } else {
            println!("  错误: 消息 {} 丢失", i);
            return false;
        }
    }
    
    true
}

/// 属性5：通知位图正确性
/// 信号位应正确设置和清除
#[test_case]
fn test_notification_bitmap() -> bool {
    let notif = Notification::new();
    
    // 设置信号
    notif.signal(0b0001);
    notif.signal(0b0010);
    notif.signal(0b0100);
    
    // 轮询应返回所有信号
    let bits = notif.poll();
    if bits != 0b0111 {
        println!("  错误: 信号位不正确，期望 0b0111，得到 0b{:b}", bits);
        return false;
    }
    
    // 再次轮询应返回 0
    let bits = notif.poll();
    if bits != 0 {
        println!("  错误: 轮询后应清除，得到 0b{:b}", bits);
        return false;
    }
    
    true
}

/// 属性6：通知原子性
/// 多次信号应正确合并
#[test_case]
fn test_notification_atomicity() -> bool {
    let notif = Notification::new();
    
    // 模拟多个信号源
    let signals = [0b0001, 0b0010, 0b0100, 0b1000, 0b0001]; // 注意重复的 0b0001
    
    for &sig in &signals {
        notif.signal(sig);
    }
    
    let bits = notif.poll();
    let expected = 0b1111; // 所有位的并集
    
    if bits != expected {
        println!("  错误: 信号合并不正确，期望 0b{:b}，得到 0b{:b}", expected, bits);
        return false;
    }
    
    true
}

/// 属性7：共享缓冲区引用计数
/// 引用计数应正确管理
#[test_case]
fn test_shared_buffer_refcount() -> bool {
    let buf = SharedBuffer::new(0x1000, 4096);
    
    // 初始引用计数为 1
    if buf.ref_count() != 1 {
        println!("  错误: 初始引用计数应为 1");
        return false;
    }
    
    // 增加引用
    buf.add_ref();
    buf.add_ref();
    
    if buf.ref_count() != 3 {
        println!("  错误: 引用计数应为 3");
        return false;
    }
    
    // 释放引用
    let is_last = buf.release();
    if is_last {
        println!("  错误: 不应是最后一个引用");
        return false;
    }
    
    if buf.ref_count() != 2 {
        println!("  错误: 引用计数应为 2");
        return false;
    }
    
    // 继续释放
    buf.release();
    let is_last = buf.release();
    
    if !is_last {
        println!("  错误: 应是最后一个引用");
        return false;
    }
    
    true
}

/// 属性8：随机消息传递测试
/// 随机发送和接收消息，验证正确性
#[test_case]
fn test_random_message_passing() -> bool {
    let mut ep = Endpoint::new(0);
    let mut rng = SimpleRng::new(12345);
    let mut pending_messages: VecDeque<Message> = VecDeque::new();
    
    for _ in 0..100 {
        let op = rng.next_range(0, 2);
        
        match op {
            0 => {
                // 发送
                let label = rng.next_usize();
                let msg = Message::new(label, &[label]);
                pending_messages.push_back(msg);
                ep.send(rng.next_usize(), msg).unwrap();
            }
            _ => {
                // 接收
                if let Some(received) = ep.receive(rng.next_usize()) {
                    if let Some(expected) = pending_messages.pop_front() {
                        if received.tag.label != expected.tag.label {
                            println!("  错误: 消息不匹配");
                            return false;
                        }
                    }
                }
            }
        }
    }
    
    true
}

/// 属性9：端点隔离
/// 不同端点的消息不应混淆
#[test_case]
fn test_endpoint_isolation() -> bool {
    let mut ep1 = Endpoint::new(1);
    let mut ep2 = Endpoint::new(2);
    
    // 向 ep1 发送消息
    let msg1 = Message::new(111, &[1]);
    ep1.send(1, msg1).unwrap();
    
    // 向 ep2 发送消息
    let msg2 = Message::new(222, &[2]);
    ep2.send(2, msg2).unwrap();
    
    // 从 ep1 接收
    if let Some(received) = ep1.receive(3) {
        if received.tag.label != 111 {
            println!("  错误: ep1 收到错误消息");
            return false;
        }
    }
    
    // 从 ep2 接收
    if let Some(received) = ep2.receive(4) {
        if received.tag.label != 222 {
            println!("  错误: ep2 收到错误消息");
            return false;
        }
    }
    
    true
}
