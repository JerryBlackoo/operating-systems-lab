#include "runtime.h"

#include <stddef.h>

int64_t user_ipc_send(uint32_t endpoint, const struct abi_message *message)
{
    return abi_decode_status(abi_ecall(ABI_IPC_SEND, abi_message_args(endpoint, message)));
}

int64_t user_ipc_reply(uint32_t reply, const struct abi_message *message)
{
    return abi_decode_status(abi_ecall(ABI_IPC_REPLY, abi_message_args(reply, message)));
}

int64_t user_ipc_recv(uint32_t endpoint, struct abi_message *message)
{
    return abi_decode_result(abi_ecall(ABI_IPC_RECV, (struct abi_regs){{endpoint}}), message);
}

int64_t user_ipc_call(uint32_t endpoint, const struct abi_message *request, struct abi_message *reply)
{
    return abi_decode_result(abi_ecall(ABI_IPC_CALL, abi_message_args(endpoint, request)), reply);
}

int64_t user_cap_mint_to(uint32_t source, uint32_t target_sid, uint16_t rights,
                         uint16_t badge, uint32_t *handle)
{
    struct abi_message result = {0};
    int64_t status = abi_decode_result(
        abi_ecall(ABI_CAP_MINT_TO,
                  (struct abi_regs){{source, target_sid, rights, badge}}),
        &result);

    if (status == ABI_OK && handle != NULL) {
        *handle = (uint32_t)result.words[0];
    }
    return status;
}

int64_t user_cap_delete(uint32_t handle)
{
    return abi_decode_status(abi_ecall(ABI_CAP_DELETE,
                                       (struct abi_regs){{handle}}));
}

/*
 * 原始块读（策略层在 U-mode）。
 *
 * ext4/vfs 现在跑在 `fs` 服务里，内核只提供块设备驱动。这个包装把一个
 * 逻辑块直接读进调用者的用户缓冲区；路径解析、目录遍历、inode/extent
 * 解码都由调用方自己做。
 */
int64_t user_block_read(uint64_t lba, void *dst, size_t blocks, size_t *bytes_out)
{
    struct abi_message result = {0};
    int64_t status = abi_decode_result(
        abi_ecall(ABI_BLOCK_READ,
                  (struct abi_regs){{lba, (uintptr_t)dst, blocks, 0, 0, 0}}),
        &result);

    if (status == ABI_OK && bytes_out != NULL) {
        *bytes_out = (size_t)result.words[0];
    }
    return status;
}

int64_t user_console_read(void *dst, size_t max, size_t *n_out)
{
    struct abi_message result = {0};
    int64_t status = abi_decode_result(
        abi_ecall(ABI_CONSOLE_READ,
                  (struct abi_regs){{(uintptr_t)dst, max, 0, 0, 0, 0}}),
        &result);

    if (status == ABI_OK && n_out != NULL) {
        *n_out = (size_t)result.words[0];
    }
    return status;
}

int64_t user_console_write(const void *src, size_t len)
{
    return abi_decode_status(
        abi_ecall(ABI_CONSOLE_WRITE,
                  (struct abi_regs){{(uintptr_t)src, len, 0, 0, 0, 0}}));
}

int64_t user_pid_self(uint64_t *pid)
{
    struct abi_message result = {0};
    int64_t st = abi_decode_result(
        abi_ecall(ABI_PID_SELF, (struct abi_regs){0}), &result);

    if (st == ABI_OK && pid != NULL) {
        *pid = result.words[0];
    }
    return st;
}

_Noreturn void user_poweroff(void)
{
    abi_ecall(ABI_POWEROFF, (struct abi_regs){0});
    /*
     * 只有 shell SID 能真正下电。被拒绝时用线程退出结束调用者，
     * 不能用非法指令把 S-mode 打进 unexpected trap。
     */
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    for (;;) {
    }
}

int64_t user_as_self(uint64_t *root_pfn)
{
    struct abi_message result = {0};
    int64_t st = abi_decode_result(
        abi_ecall(ABI_AS_SELF, (struct abi_regs){0}), &result);

    if (st == ABI_OK && root_pfn != NULL) {
        *root_pfn = result.words[0];
    }
    return st;
}

int64_t user_pager_done(uint64_t fault_tid)
{
    return abi_decode_status(abi_ecall(ABI_PAGER_DONE,
                                       (struct abi_regs){{fault_tid}}));
}

int64_t user_pager_fail(uint64_t fault_tid, int64_t status)
{
    return abi_decode_status(
        abi_ecall(ABI_PAGER_FAIL,
                  (struct abi_regs){{fault_tid, (uint64_t)status}}));
}

int64_t user_pager_ready(void)
{
    return abi_decode_status(abi_ecall(ABI_PAGER_READY, (struct abi_regs){0}));
}

int64_t user_reply_caller_sid(uint32_t reply, uint8_t *sid)
{
    struct abi_message result = {0};
    int64_t st = abi_decode_result(
        abi_ecall(ABI_REPLY_CALLER_SID, (struct abi_regs){{reply, 0, 0, 0, 0, 0}}),
        &result);

    if (st == ABI_OK && sid != NULL) {
        *sid = (uint8_t)result.words[0];
    }
    return st;
}

__attribute__((weak)) void *memcpy(void *dst, const void *src, size_t n)
{
    uint8_t *out = dst;
    const uint8_t *in = src;

    for (size_t i = 0; i < n; i++) {
        out[i] = in[i];
    }
    return dst;
}

__attribute__((weak)) void *memset(void *dst, int value, size_t n)
{
    uint8_t *out = dst;

    for (size_t i = 0; i < n; i++) {
        out[i] = (uint8_t)value;
    }
    return dst;
}

__attribute__((weak)) int memcmp(const void *left, const void *right, size_t n)
{
    const uint8_t *a = left;
    const uint8_t *b = right;

    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i]) {
            return (int)a[i] - (int)b[i];
        }
    }
    return 0;
}

_Noreturn void user_start(const struct start_info *info, void (*app_main)(const struct start_info *))
{
    app_main(info);
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    __builtin_trap();
}