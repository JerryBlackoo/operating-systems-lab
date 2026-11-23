#ifndef MICRO_MODEL_RUNTIME_H
#define MICRO_MODEL_RUNTIME_H

#include "abi/abi.h"

struct start_info { uintptr_t argument; };

_Noreturn void user_start(const struct start_info *info, void (*app_main)(const struct start_info *));
int64_t user_ipc_send(uint32_t endpoint, const struct abi_message *message);
int64_t user_ipc_reply(uint32_t reply, const struct abi_message *message);
int64_t user_ipc_recv(uint32_t endpoint, struct abi_message *message);
int64_t user_ipc_call(uint32_t endpoint, const struct abi_message *request, struct abi_message *reply);

#endif
