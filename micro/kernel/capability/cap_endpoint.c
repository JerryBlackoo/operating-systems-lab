#include "cap_endpoint.h"

#include "ipc.h"

static int endpoint_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    (void)obj;
    (void)op;
    (void)args;
    return CAP_E_TYPE;
}

static void endpoint_finalize(struct cap_object *obj)
{
    if (obj != NULL && obj->payload != NULL) {
        ep_unref((struct endpoint *)obj->payload);
        obj->payload = NULL;
    }
}

static const struct cap_object_ops endpoint_ops = {
    .invoke = endpoint_invoke,
    .finalize = endpoint_finalize,
};

const struct cap_type cap_endpoint_type = {
    .type = CAP_TYPE_ENDPOINT,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
    .supported = (1u << CAP_OP_SEND) | (1u << CAP_OP_RECV),
    .required = {
        [CAP_OP_SEND] = CAP_RIGHT_WRITE,
        [CAP_OP_RECV] = CAP_RIGHT_READ,
    },
    .ops = &endpoint_ops,
};

void cap_endpoint_register_type(void)
{
    /* 类型表在 capability.c 内静态注册；此处仅占位，供 resident 显式链接。 */
}