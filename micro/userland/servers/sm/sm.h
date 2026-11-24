#ifndef MICRO_MODEL_SM_H
#define MICRO_MODEL_SM_H

#include <stddef.h>
#include <stdint.h>

/*
 * sm —— 服务名称注册（service manager）。
 *
 * 只维护 Name ⇀ (owner, endpoint_ref) 的映射和登记状态：
 *
 *   未注册 ──register──> 已注册 ──owner unregister──> 未注册
 *                          └──lookup──> 向客户端能力空间发放发送句柄
 *
 * 端点引用与句柄如何表示、权限如何检查，全部由 sm_port 后面的能力模块
 * 决定；sm 不实现能力体系。
 */

enum { SM_CAPACITY = 4, SM_NAME_MAX = 32 };

enum sm_result {
    SM_OK = 0,
    SM_BAD_NAME,
    SM_EXISTS,
    SM_NOT_FOUND,
    SM_NO_SPACE,
    SM_DENIED,
    SM_BAD_ARG,
};

enum sm_state { SM_UNREGISTERED = 0, SM_REGISTERED };

struct sm_port {
    void *ctx;
    /* 为 owner 持有的端点 handle 增加一个注册项引用，返回稳定的端点引用。 */
    enum sm_result (*retain)(void *ctx, uint32_t owner, uint32_t handle,
                             uint32_t *endpoint);
    /* 向 client 自己的能力空间发放仅可向 endpoint 发送的句柄。 */
    enum sm_result (*grant_send)(void *ctx, uint32_t client, uint32_t endpoint,
                                 uint32_t *handle);
    /* 释放注册项持有的端点引用；客户端句柄的引用由客户端自己释放。 */
    void (*release)(void *ctx, uint32_t endpoint);
};

struct sm_binding {
    enum sm_state state;
    uint32_t owner;
    uint32_t endpoint;
    size_t length;
    char name[SM_NAME_MAX];
};

struct sm {
    const struct sm_port *port;
    struct sm_binding names[SM_CAPACITY];
};

/* 名称：非空、至多 SM_NAME_MAX 字节、逐字节比较（区分大小写）。 */
enum sm_result sm_register(struct sm *sm, uint32_t owner, const char *name,
                           size_t length, uint32_t handle);
enum sm_result sm_lookup(struct sm *sm, uint32_t client, const char *name,
                         size_t length, uint32_t *handle);
enum sm_result sm_unregister(struct sm *sm, uint32_t owner, const char *name,
                             size_t length);

/* 发布者退出：回收 owner 名下的全部注册项，返回回收数量。 */
unsigned sm_owner_exit(struct sm *sm, uint32_t owner);

/* 当前有效注册项数；用于自检和观测。 */
unsigned sm_count(const struct sm *sm);

#endif /* MICRO_MODEL_SM_H */
