#ifndef MICRO_MODEL_ABI_H
#define MICRO_MODEL_ABI_H

/*
 * shared/abi/abi.h — RV64 ecall 二进制约定
 *
 * 【v0 模型草案】
 *
 * 本文件只约定一次 ecall 的寄存器向量和消息位布局：
 *   - a7      调用号
 *   - a0..a5  输入参数；返回时 a0 为状态，a1 为 meta，a2..a5 为消息字
 *   - a6      保留
 *
 * 同时提供内核分发层与用户态包装层共用的编解码和形状校验。具体调用号的
 * 参数个数、句柄类型、对象权限和阻塞语义由 kernel/syscall 与各对象模块定义。
 *
 * 位宽或编号变更会同时破坏内核与用户态，必须重建两侧。
 */

#include <stdbool.h>
#include <stdint.h>

enum abi_number {
    ABI_YIELD = 1,
    ABI_THREAD_EXIT = 2,
    ABI_IPC_SEND = 3,
    ABI_IPC_RECV = 4,
    ABI_IPC_CALL = 5,
    ABI_IPC_REPLY = 6,
    ABI_AS_MAP = 7,
    ABI_AS_UNMAP = 8,
    /* M3：仅 SM/PM 服务进程可用的内核协助调用。 */
    ABI_CAP_MINT_TO = 9,
    ABI_PM_SPAWN = 10,
    ABI_CAP_DELETE = 11,

    /*
     * 编号 13 曾是 ABI_VFS_READ_AT（内核解析路径后读文件）。
     *
     * A1 之后 ext4/vfs 已迁到 U-mode `fs` 服务，内核不再解释路径，
     * 该调用被删除并留下空洞（与 12/18 一样，表里为 NULL handler）。
     * 现在的等价能力是 ABI_BLOCK_READ（原始块读）。
     */
    /* M7：仅 console 服务可访问 UART 行队列/输出。 */
    ABI_CONSOLE_READ = 14,
    ABI_CONSOLE_WRITE = 15,
    /* M7：仅 shell 可请求整机下电。 */
    ABI_POWEROFF = 16,
    /* M7：返回当前线程 object id。 */
    ABI_PID_SELF = 17,
    /* M6：仅 VM 服务可代表目标进程补页 / 唤醒缺页线程。 */
    ABI_VM_MAP = 19,
    ABI_PAGER_DONE = 20,
    /* M6：返回调用线程所在 AS 的根页帧；任意 U-mode 线程可读自己。 */
    ABI_AS_SELF = 21,
    /* M6：仅 VM 服务可拒绝一次缺页，缺页线程收到明确错误并退出。 */
    ABI_PAGER_FAIL = 22,
    /* M6：VM 服务进入缺页循环前宣告 pager 可用（早于 SM 注册之外的服务启动）。 */
    ABI_PAGER_READY = 23,
    /*
     * 查询某次 in-flight call 的真实发起者 sid（由内核在 ep_call 时从 TCB
     * 记录，不读任何消息字段）。仅 SM 可用。
     *
     * 存在理由：ipc_message 里没有发送者身份字段，SM 原先只能相信消息里
     * 自称的 sid，于是任意 U-mode 进程都能冒充别的 sid，让 SM 把能力
     * mint 进受害者 CSpace 或冒名注册服务名。SM 改为用本调用取真实身份。
     */
    ABI_REPLY_CALLER_SID = 24,
    /*
     * PM 在 spawn 后把新进程的 AS MAP 权交给 VM（cap_vm_ensure_as_map）。
     * 仅 SERVICE_SID_PM；a0=线程 object id。与 loader/pager 启动路径互补。
     */
    ABI_VM_GRANT_AS = 25,
    /*
     * PM 在 spawn 后续步骤（如 VM grant）失败时撤回子线程与固定 SID 上的
     * spawn cap（子线程须尚未 RUNNING）。仅 SERVICE_SID_PM。
     */
    ABI_PM_DISCARD = 26,
    /*
     * 原始块读（策略层在 U-mode）。
     *
     * ext4/vfs 已迁到 U-mode `fs` 服务，内核只保留块设备驱动。本调用把
     * 一个逻辑块读进调用者的用户缓冲区：
     *   a0 = lba, a1 = 用户目标地址, a2 = 块数（1..N）
     * 成功时 words[0] = 实际读到的字节数。
     *
     * 仅 SERVICE_SID_FS。语义是「读原始扇区」：内核不做任何文件系统解释，
     * 因此路径解析、目录遍历、权限判断全都留在用户态。
     */
    ABI_BLOCK_READ = 27,
    /*
     * 块设备信息：words[0]=容量（块数）words[1]=块大小（字节）。
     * 仅 SERVICE_SID_FS。U-mode `fs` 挂 ext4 时需要容量来校验超级块与
     * extent 边界；内核不解释这些值，只是如实上报驱动探测结果。
     */
    ABI_BLOCK_INFO = 28,
};

#define MICRO_ABI_MAX_NR ABI_BLOCK_INFO

enum abi_status {
    ABI_OK = 0,
    ABI_BAD_NR = -1,
    ABI_BAD_ARG = -2,
    ABI_BAD_HANDLE = -3,
    ABI_DENIED = -4,
    ABI_NO_MEMORY = -5,
    ABI_CLOSED = -6,
    ABI_ALREADY_MAPPED = -7,
    ABI_NOT_MAPPED = -8,
    ABI_BUSY = -9,
    ABI_NOT_FOUND = -10,
    /* 服务端在限定时间内没有应答（pager 缺页超时等）。 */
    ABI_TIMED_OUT = -11,
};

enum {
    ABI_READ = 1,
    ABI_WRITE = 2,
    ABI_EXEC = 4,
    ABI_RIGHTS_MASK = ABI_READ | ABI_WRITE | ABI_EXEC,
    ABI_PAGE_SIZE = 4096,
    ABI_WORDS = 4,
    ABI_REGS = 6, /* a0..a5；a6 保留，a7 单独承载调用号 */
};

/* meta[31:0] = tag，meta[63:32] = reply。 */
#define ABI_META_TAG_SHIFT 0
#define ABI_META_REPLY_SHIFT 32

struct abi_regs { uint64_t a[ABI_REGS]; };

struct abi_message {
    uint32_t tag;
    uint32_t reply;
    uint64_t words[ABI_WORDS];
};

_Static_assert(ABI_REGS == 6, "ecall vector carries a0..a5");
_Static_assert(ABI_WORDS == 4, "message carries four words");
_Static_assert(sizeof(struct abi_regs) == ABI_REGS * sizeof(uint64_t),
               "abi_regs must stay a plain 6-word vector");
_Static_assert(sizeof(struct abi_message) == 8 + ABI_WORDS * sizeof(uint64_t),
               "abi_message layout changed");
_Static_assert(__builtin_offsetof(struct abi_message, reply) == 4,
               "reply must be the high half of meta");
_Static_assert(__builtin_offsetof(struct abi_message, words) == 8,
               "words must follow tag/reply");

static inline uint64_t abi_meta(uint32_t tag, uint32_t reply)
{
    return ((uint64_t)reply << ABI_META_REPLY_SHIFT) |
           ((uint64_t)tag << ABI_META_TAG_SHIFT);
}

static inline uint32_t abi_meta_tag(uint64_t meta)
{
    return (uint32_t)(meta >> ABI_META_TAG_SHIFT);
}

static inline uint32_t abi_meta_reply(uint64_t meta)
{
    return (uint32_t)(meta >> ABI_META_REPLY_SHIFT);
}

/*
 * 输入向量：a0 = 句柄/首参，a1 = 32 位 tag（零扩展），a2..a5 = 四个字。
 * 发送方向 a1 不是完整 meta；reply 只出现在接收结果的 meta 高 32 位。
 */
static inline struct abi_regs abi_message_args(uint32_t handle, const struct abi_message *msg)
{
    return (struct abi_regs){{
        handle, msg->tag, msg->words[0], msg->words[1], msg->words[2], msg->words[3],
    }};
}

static inline struct abi_message abi_decode_message(struct abi_regs regs)
{
    return (struct abi_message){
        .tag = abi_meta_tag(regs.a[1]),
        .reply = abi_meta_reply(regs.a[1]),
        .words = {regs.a[2], regs.a[3], regs.a[4], regs.a[5]},
    };
}

/* a0 是符号扩展的 64 位状态；避开实现定义的 uint64 -> int64 转换。 */
static inline int64_t abi_decode_status(struct abi_regs regs)
{
    uint64_t word = regs.a[0];

    if (word <= INT64_MAX)
        return (int64_t)word;
    return -1 - (int64_t)(UINT64_MAX - word);
}

/*
 * 结果向量：成功且带消息时填 meta 和四个字；错误或无消息时 a1..a5 全零。
 * 内核分发层只通过这里产生返回向量。
 */
static inline struct abi_regs abi_result(int64_t status, const struct abi_message *msg)
{
    struct abi_regs regs = {{(uint64_t)status}};

    if (status == ABI_OK && msg) {
        regs.a[1] = abi_meta(msg->tag, msg->reply);
        for (unsigned i = 0; i < ABI_WORDS; i++)
            regs.a[i + 2] = msg->words[i];
    }
    return regs;
}

/*
 * 用户侧结果解码：只有成功状态才把向量解释为消息。
 * 错误时保持调用者原有的输出内容不变。
 */
static inline int64_t abi_decode_result(struct abi_regs regs, struct abi_message *message)
{
    int64_t status = abi_decode_status(regs);

    if (status == ABI_OK && message)
        *message = abi_decode_message(regs);
    return status;
}

/* ---- 两侧共用的向量形状校验（不理解具体调用语义） ---- */

/* 从 first 起的所有参数必须为零。 */
static inline bool abi_tail_zero(struct abi_regs regs, unsigned first)
{
    for (unsigned i = first; i < ABI_REGS; i++) {
        if (regs.a[i])
            return false;
    }
    return true;
}

/* 32 位句柄/标签/帧号按零扩展传入；高位非零即形状错误。 */
static inline bool abi_arg_u32(struct abi_regs regs, unsigned index, uint32_t *out)
{
    if (index >= ABI_REGS || regs.a[index] > UINT32_MAX)
        return false;
    if (out)
        *out = (uint32_t)regs.a[index];
    return true;
}

static inline bool abi_page_aligned(uint64_t va)
{
    return (va & (ABI_PAGE_SIZE - 1)) == 0;
}

/* 低三位是 R/W/X；至少一位，可写映射必须可读且不得可执行（W^X）。 */
static inline bool abi_rights_valid(uint64_t rights)
{
    return rights != 0 && rights <= ABI_RIGHTS_MASK &&
           (!(rights & ABI_WRITE) ||
            ((rights & ABI_READ) && !(rights & ABI_EXEC)));
}

#if defined(__riscv)
static inline struct abi_regs abi_ecall(uint64_t nr, struct abi_regs input)
{
    register uint64_t a0 __asm__("a0") = input.a[0];
    register uint64_t a1 __asm__("a1") = input.a[1];
    register uint64_t a2 __asm__("a2") = input.a[2];
    register uint64_t a3 __asm__("a3") = input.a[3];
    register uint64_t a4 __asm__("a4") = input.a[4];
    register uint64_t a5 __asm__("a5") = input.a[5];
    register uint64_t a7 __asm__("a7") = nr;

    __asm__ volatile("ecall"
                     : "+r"(a0), "+r"(a1), "+r"(a2), "+r"(a3), "+r"(a4), "+r"(a5)
                     : "r"(a7) : "memory");
    return (struct abi_regs){{a0, a1, a2, a3, a4, a5}};
}
#endif

#endif
