#include "bootstrap.h"
#include "demo.h"

extern const struct demo_module *const bootstrap_plan[];

void bootstrap_run(void)
{
    demo_log("[bootstrap] begin");
    for (const struct demo_module *const *entry = bootstrap_plan; *entry; entry++)
        demo_run(*entry);
    demo_log("[bootstrap] PASS");
}
