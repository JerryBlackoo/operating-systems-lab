#ifndef MICRO_MODEL_INIT_H
#define MICRO_MODEL_INIT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum {
    INIT_CAPACITY = 4,
    INIT_BAD_PLAN = -1,
    INIT_ALREADY_STARTED = -2,
    INIT_BAD_PORT = -3,
    INIT_BAD_START = -4,
};
enum init_phase { INIT_BOOT, INIT_STARTING, INIT_READY, INIT_FAILED };

struct init_service { uint32_t id, grants; };
struct init_child { uint32_t handle; uint64_t ready_receipt; };

/*
 * 端口是唯一的外部依赖：start 创建清单项对应的子服务并返回本项专用的
 * 非零 handle / ready_receipt；失败返回非零状态。stop 释放该项资源。
 * 两项都必须与成功启动的记录一一对应，cleanup 按启动的逆序调用 stop。
 */
struct init_port {
    void *ctx;
    int (*start)(void *, const struct init_service *, struct init_child *);
    void (*stop)(void *, uint32_t handle);
};

struct init_model {
    enum init_phase phase;
    const struct init_port *port;
    struct init_service plan[INIT_CAPACITY];
    struct init_child started[INIT_CAPACITY];
    size_t count, started_count;
    uint32_t failed_service;
    int error;
};

int init_boot(struct init_model *, const struct init_port *, uint32_t boot_caps,
              const struct init_service *, size_t count);
/* 接受当前等待项的凭据；返回 true 只表示凭据匹配，下一项仍可能启动失败。 */
bool init_ready(struct init_model *, uint64_t receipt);
bool init_on_exit(struct init_model *, uint32_t child, int status);

#endif
