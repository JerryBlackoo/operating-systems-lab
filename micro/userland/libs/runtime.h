#ifndef MICRO_MODEL_RUNTIME_H
#define MICRO_MODEL_RUNTIME_H

#include <stddef.h>

#include "abi/abi.h"

struct start_info { uintptr_t argument; };

_Noreturn void user_start(const struct start_info *info, void (*app_main)(const struct start_info *));
int64_t user_ipc_send(uint32_t endpoint, const struct abi_message *message);
int64_t user_ipc_reply(uint32_t reply, const struct abi_message *message);
int64_t user_ipc_recv(uint32_t endpoint, struct abi_message *message);
int64_t user_ipc_call(uint32_t endpoint, const struct abi_message *request, struct abi_message *reply);
int64_t user_cap_mint_to(uint32_t source, uint32_t target_sid, uint16_t rights,
                         uint16_t badge, uint32_t *handle);
int64_t user_cap_delete(uint32_t handle);
/*
 * 原始块读：ext4/vfs 已迁到 U-mode，内核只提供块设备驱动。
 * 成功时 *bytes_out 为实际读到的字节数。
 */
int64_t user_block_read(uint64_t lba, void *dst, size_t blocks, size_t *bytes_out);
int64_t user_console_read(void *dst, size_t max, size_t *n_out);
int64_t user_console_write(const void *src, size_t len);
int64_t user_pid_self(uint64_t *pid);
_Noreturn void user_poweroff(void);
int64_t user_as_self(uint64_t *root_pfn);
int64_t user_pager_done(uint64_t fault_tid);
int64_t user_pager_fail(uint64_t fault_tid, int64_t status);
int64_t user_pager_ready(void);
/*
 * 查询某次 in-flight call 的真实发起者 sid。仅 SM 可用（内核按 caller 校验）。
 * 服务端不得相信消息里自带的 sid 字段——那是调用者可以随便填的。
 */
int64_t user_reply_caller_sid(uint32_t reply, uint8_t *sid);

#endif