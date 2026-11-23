#ifndef MICRO_MODEL_ECHO_H
#define MICRO_MODEL_ECHO_H

#include <stdbool.h>
#include "abi/abi.h"

enum { ECHO_REQUEST = 1, ECHO_REPLY = 2 };
enum echo_state { ECHO_READY, ECHO_WAITING, ECHO_DONE, ECHO_FAILED };

struct echo_port {
    void *ctx;
    int64_t (*call)(void *, uint32_t, const struct abi_message *);
    void (*report)(void *, enum echo_state);
    void (*release)(void *, uint32_t);
};

struct echo_app {
    const struct echo_port *port;
    uint32_t endpoint;
    uint64_t value;
    enum echo_state state;
};

bool echo_start(struct echo_app *app);
bool echo_on_reply(struct echo_app *app, int64_t status, const struct abi_message *reply);

#endif
