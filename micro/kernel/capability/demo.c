#include "capability.h"
#include "demo.h"

static void run(void)
{
    CHECK(cap_selftest() == 0);
}

DEMO_MODULE(capability, false, run, NULL);
