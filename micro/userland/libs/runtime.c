#include "runtime.h"

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

_Noreturn void user_start(const struct start_info *info, void (*app_main)(const struct start_info *))
{
    app_main(info);
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    __builtin_trap();
}
