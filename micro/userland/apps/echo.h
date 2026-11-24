#ifndef MICRO_MODEL_ECHO_H
#define MICRO_MODEL_ECHO_H

#include <stdbool.h>
#include "abi/abi.h"

enum { ECHO_REQUEST = 1, ECHO_REPLY = 2 };
enum echo_state { ECHO_READY, ECHO_WAITING, ECHO_DONE, ECHO_FAILED };

/*
 * call 与运行库的 user_ipc_call 同构：同步提交请求，成功时填写应答。
 * report 报告本轮唯一的结果，release 交还预置的端点句柄。
 */
struct echo_port {
    void *ctx;
    int64_t (*call)(void *, uint32_t, const struct abi_message *, struct abi_message *reply);
    void (*report)(void *, enum echo_state);
    void (*release)(void *, uint32_t);
};

struct echo_app {
    const struct echo_port *port;
    uint32_t endpoint;
    uint64_t value;
    enum echo_state state;
};

/* READY -> WAITING，收到应答后经 echo_on_reply 进入 DONE 或 FAILED。 */
bool echo_start(struct echo_app *app);
bool echo_on_reply(struct echo_app *app, int64_t status, const struct abi_message *reply);

#endif
