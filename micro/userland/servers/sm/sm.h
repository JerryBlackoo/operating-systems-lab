#ifndef MICRO_MODEL_SM_H
#define MICRO_MODEL_SM_H

#include <stddef.h>
#include <stdint.h>

enum { SM_CAPACITY = 4, SM_NAME_MAX = 32 };
enum sm_result { SM_OK, SM_BAD_NAME, SM_EXISTS, SM_NOT_FOUND, SM_NO_SPACE, SM_DENIED, SM_BAD_ARG };

struct sm_port {
    void *ctx;
    enum sm_result (*retain)(void *, uint32_t owner, uint32_t handle, uint32_t *endpoint);
    enum sm_result (*grant_send)(void *, uint32_t client, uint32_t endpoint, uint32_t *handle);
    void (*release)(void *, uint32_t endpoint);
};

struct sm_binding {
    uint32_t owner, endpoint;
    size_t length;
    char name[SM_NAME_MAX];
};

struct sm { const struct sm_port *port; struct sm_binding names[SM_CAPACITY]; };

enum sm_result sm_register(struct sm *, uint32_t owner, const char *, size_t, uint32_t handle);
enum sm_result sm_lookup(struct sm *, uint32_t client, const char *, size_t, uint32_t *handle);
enum sm_result sm_unregister(struct sm *, uint32_t owner, const char *, size_t);

#endif
