#include "init.h"
#include "demo.h"

struct launcher { unsigned live, starts, stops; uint32_t fail_id, stopped[2]; };

static int start(void *ctx, const struct init_service *service, struct init_child *child)
{
    struct launcher *launcher = ctx;
    CHECK(service->grants == (1u << (service->id - 1)));
    launcher->starts++;
    if (service->id == launcher->fail_id)
        return -99;
    launcher->live++;
    *child = (struct init_child){.handle = service->id, .ready_receipt = 100 + service->id};
    return 0;
}

static void stop(void *ctx, uint32_t handle)
{
    struct launcher *launcher = ctx;
    CHECK(launcher->live && launcher->stops < 2);
    launcher->live--;
    launcher->stopped[launcher->stops++] = handle;
}

static void run(void)
{
    const struct init_service plan[] = {{1, 1}, {2, 2}};
    struct launcher launcher = {0};
    const struct init_port port = {.ctx = &launcher, .start = start, .stop = stop};
    struct init_model init = {0};

    CHECK(init_boot(&init, &port, 3, plan, 2) == 0);
    CHECK(init.phase == INIT_STARTING && launcher.starts == 1);
    CHECK(!init_ready(&init, 102));
    CHECK(init_ready(&init, 101) && launcher.starts == 2);
    CHECK(!init_ready(&init, 101));
    CHECK(init_ready(&init, 102) && init.phase == INIT_READY && launcher.live == 2);

    init = (struct init_model){0};
    launcher = (struct launcher){.fail_id = 2};
    CHECK(init_boot(&init, &port, 3, plan, 2) == 0);
    CHECK(init_ready(&init, 101) && init.phase == INIT_FAILED);
    CHECK(init.failed_service == 2 && init.error == -99);
    CHECK(launcher.live == 0 && launcher.stops == 1 && launcher.stopped[0] == 1);

    init = (struct init_model){0};
    launcher = (struct launcher){0};
    CHECK(init_boot(&init, &port, 3, plan, 2) == 0 && init_ready(&init, 101));
    CHECK(init_on_exit(&init, 2, 17) && init.phase == INIT_FAILED);
    CHECK(launcher.stopped[0] == 2 && launcher.stopped[1] == 1 && launcher.live == 0);

    init = (struct init_model){0};
    CHECK(init_boot(&init, &port, 0, NULL, 0) == 0 && init.phase == INIT_READY);
    init = (struct init_model){0};
    launcher = (struct launcher){0};
    CHECK(init_boot(&init, &port, 1, plan, 2) == INIT_BAD_PLAN && launcher.starts == 0);
    demo_log("startup: ordered readiness, early exit and reverse cleanup");
}

DEMO_MODULE(init, true, run, NULL);
