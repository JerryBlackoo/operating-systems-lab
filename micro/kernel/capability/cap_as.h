#ifndef MICRO_KERNEL_CAP_AS_H
#define MICRO_KERNEL_CAP_AS_H

#include <stdbool.h>

#include "capability.h"
#include "memory.h"

#define CAP_TYPE_AS 2

extern const struct cap_type cap_as_type;

void cap_as_register_type(void);

/* 为 AS 建立 cap 对象（幂等）；须在 cap_system_init 之后调用。 */
int cap_as_bind(struct as *as);

/* VM 是否持有该 AS 的 MAP 权。 */
bool cap_vm_can_map_as(const struct as *as);

/* 缺页/装载路径：确保 SERVICE_SID_VM 持目标 AS 的 MAP cap。 */
int cap_vm_ensure_as_map(struct as *as);

/* PM spawn：把 AS 的 MAP+GRANT 源 cap 装进 PM CSpace（返回句柄）。 */
int cap_pm_install_as_grant(struct as *as, cap_handle_t *out);

/* PM 用源 cap mint 减权 MAP 能力给 VM；成功后可删 PM 侧源 cap。 */
int cap_pm_mint_vm_map(cap_sid_t pm_sid, cap_handle_t src, const struct as *as);

/* AS 销毁时尝试丢弃 cap 对象（VM 仍持有 cap 时可能失败）。 */
void cap_as_release(struct as *as);

#endif