#include "bootstrap.h"
#include "demo.h"

extern const struct demo_module *const bootstrap_plan[];

/*
 * 清单由构建期生成，以 NULL 结尾。这里只校验描述符本身；
 * 每项的 [name] start / [name] PASS 由 demo_run 统一输出。
 */
void bootstrap_run(void)
{
    const struct demo_module *const *entry;

    for (entry = bootstrap_plan; *entry != NULL; entry++) {
        if ((*entry)->name == NULL || (*entry)->run == NULL)
            demo_fail("invalid bootstrap module", __LINE__);
    }

    demo_log("[bootstrap] begin");
    for (entry = bootstrap_plan; *entry != NULL; entry++)
        demo_run(*entry);
    demo_log("[bootstrap] PASS");
}
