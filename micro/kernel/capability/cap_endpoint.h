#ifndef MICRO_KERNEL_CAP_ENDPOINT_H
#define MICRO_KERNEL_CAP_ENDPOINT_H

#include "capability.h"

#define CAP_TYPE_ENDPOINT 1

/* 注册 endpoint 的 cap 类型；需在 cap_system_init 之后调用一次。 */
void cap_endpoint_register_type(void);

extern const struct cap_type cap_endpoint_type;

#endif