#include "init.h"
#include "capability.h"
#include "pm.h"
#include "sm.h"
#include "demo.h"

enum {
    INIT_SID = 0,
    INIT_OWNER = 1,
    INIT_PID = 0x100,
    CLIENT = 2,
    RESOURCE_BASE = 0x1000,
    RECEIPT_BASE = 0x2000,
    ENDPOINT_BASE = 0x3000,
};

/* ---- 模型用例：启动器替身 ---- */

enum fake_flaw { FAKE_OK, FAKE_ZERO_HANDLE, FAKE_DUP_RECEIPT };

struct fake_launcher {
    unsigned live, starts, stops;
    uint32_t fail_id;
    enum fake_flaw flaw;
    uint32_t stopped[INIT_CAPACITY];
};

static int fake_start(void *ctx, const struct init_service *service,
                      struct init_child *child)
{
    struct fake_launcher *fake = ctx;
    CHECK(service->grants == (1u << (service->id - 1)));
    fake->starts++;
    if (service->id == fake->fail_id)
        return -99;

    struct init_child next = {.handle = service->id, .ready_receipt = 100 + service->id};
    if (fake->flaw == FAKE_ZERO_HANDLE && service->id == 1)
        next.handle = 0;
    if (fake->flaw == FAKE_DUP_RECEIPT && service->id == 2)
        next.ready_receipt = 101;
    if (next.handle) {
        CHECK(fake->live < INIT_CAPACITY);
        fake->live++;
    }
    *child = next;
    return 0;
}

static void fake_stop(void *ctx, uint32_t handle)
{
    struct fake_launcher *fake = ctx;
    CHECK(fake->live && fake->stops < INIT_CAPACITY);
    fake->live--;
    fake->stopped[fake->stops++] = handle;
}

static struct init_port fake_port(struct fake_launcher *fake)
{
    return (struct init_port){.ctx = fake, .start = fake_start, .stop = fake_stop};
}

static void fake_cases(void)
{
    const struct init_service plan[] = {{1, 1}, {2, 2}};
    const struct init_service zero_id[] = {{0, 0}};
    const struct init_service duplicate[] = {{1, 1}, {1, 1}};
    const struct init_service too_many[] = {{1, 1}, {2, 2}, {3, 4}, {4, 8}, {5, 16}};
    struct fake_launcher fake = {0};
    struct init_port port = fake_port(&fake);
    struct init_model init = {0};

    CHECK(init_boot(&init, &port, 3, plan, 2) == 0);
    CHECK(init.phase == INIT_STARTING && fake.starts == 1);
    CHECK(!init_ready(&init, 102));
    CHECK(init_ready(&init, 101) && fake.starts == 2);
    CHECK(!init_ready(&init, 101));
    CHECK(init_ready(&init, 102) && init.phase == INIT_READY && fake.live == 2);

    init = (struct init_model){0};
    fake = (struct fake_launcher){.fail_id = 2};
    port = fake_port(&fake);
    CHECK(init_boot(&init, &port, 3, plan, 2) == 0);
    CHECK(init_ready(&init, 101) && init.phase == INIT_FAILED);
    CHECK(init.failed_service == 2 && init.error == -99);
    CHECK(fake.live == 0 && fake.stops == 1 && fake.stopped[0] == 1);
    CHECK(init_boot(&init, &port, 3, plan, 2) == INIT_ALREADY_STARTED);

    init = (struct init_model){0};
    fake = (struct fake_launcher){0};
    port = fake_port(&fake);
    CHECK(init_boot(&init, &port, 3, plan, 2) == 0 && init_ready(&init, 101));
    CHECK(init_on_exit(&init, 2, 17) && init.phase == INIT_FAILED);
    CHECK(fake.stopped[0] == 2 && fake.stopped[1] == 1 && fake.live == 0);
    CHECK(!init_on_exit(&init, 1, 0));

    init = (struct init_model){0};
    CHECK(init_boot(&init, &port, 0, NULL, 0) == 0 && init.phase == INIT_READY);

    init = (struct init_model){0};
    CHECK(init_boot(&init, NULL, 3, plan, 2) == INIT_BAD_PORT);
    init = (struct init_model){0};
    CHECK(init_boot(&init, &port, 3, zero_id, 1) == INIT_BAD_PLAN);
    init = (struct init_model){0};
    CHECK(init_boot(&init, &port, 3, duplicate, 2) == INIT_BAD_PLAN);
    init = (struct init_model){0};
    CHECK(init_boot(&init, &port, 31, too_many, 5) == INIT_BAD_PLAN);
    init = (struct init_model){0};
    fake = (struct fake_launcher){0};
    port = fake_port(&fake);
    CHECK(init_boot(&init, &port, 1, plan, 2) == INIT_BAD_PLAN && fake.starts == 0);

    init = (struct init_model){0};
    fake = (struct fake_launcher){.flaw = FAKE_ZERO_HANDLE};
    port = fake_port(&fake);
    CHECK(init_boot(&init, &port, 3, plan, 2) == INIT_BAD_START);
    CHECK(init.phase == INIT_FAILED && fake.starts == 1 && fake.live == 0 &&
          fake.stops == 0);

    init = (struct init_model){0};
    fake = (struct fake_launcher){.flaw = FAKE_DUP_RECEIPT};
    port = fake_port(&fake);
    CHECK(init_boot(&init, &port, 3, plan, 2) == 0);
    CHECK(init_ready(&init, 101) && init.phase == INIT_FAILED &&
          init.error == INIT_BAD_START);
    CHECK(fake.live == 0 && fake.stops == 2 && fake.stopped[0] == 2 &&
          fake.stopped[1] == 1);
}

/* ---- 系统用例：capability + pm + sm ---- */

struct boot_service {
    uint32_t cap, as, caps, thread, pid;
    bool registered;
    const char *name;
    size_t length;
};

struct system {
    struct init_model init;
    struct pm pm;
    struct sm sm;
    struct pm_port pm_port;
    struct sm_port sm_port;
    const struct init_service *plan;
    struct boot_service services[INIT_CAPACITY];
    uint32_t root;
    uint32_t fail_id;
    unsigned current;
    unsigned starts, live, retains;
    enum task_resource release_order[3 * INIT_CAPACITY];
    unsigned releases;
    uint32_t stop_order[INIT_CAPACITY];
    unsigned stops;
};

static const char *const system_names[] = {"sm", "pm", "vm", "vfs"};

static const struct init_service system_plan[] = {
    {1, CAP_RIGHT_READ | CAP_RIGHT_GRANT},
    {2, CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT},
    {3, CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_MAP},
    {4, CAP_RIGHT_READ},
};

static int boot_invoke(struct cap_object *object, uint32_t op, void *args)
{
    (void)object;
    (void)op;
    (void)args;
    return 0;
}

static void boot_finalize(struct cap_object *object)
{
    (void)object;
}

static const struct cap_object_ops boot_ops = {
    .invoke = boot_invoke,
    .finalize = boot_finalize,
};

static const struct cap_type boot_type = {
    .type = 1,
    .rights_mask = CAP_RIGHT_ALL,
    .supported = 0,
    .required = {0},
    .ops = &boot_ops,
};

static size_t text_length(const char *text)
{
    size_t length = 0;
    while (text[length])
        length++;
    return length;
}

static size_t system_index(const struct system *sys, const struct init_service *service)
{
    for (size_t i = 0; i < INIT_CAPACITY; i++) {
        if (sys->plan[i].id == service->id)
            return i;
    }
    return INIT_CAPACITY;
}

static uint32_t resource_id(unsigned index, unsigned kind)
{
    return RESOURCE_BASE + index * 4 + kind;
}

static uint32_t pm_create_as(void *ctx, uint32_t image)
{
    struct system *sys = ctx;
    CHECK(sys->current < INIT_CAPACITY);
    struct boot_service *service = &sys->services[sys->current];
    CHECK(image == sys->plan[sys->current].id && !service->as);
    service->as = resource_id(sys->current, 1);
    return service->as;
}

static uint32_t pm_create_caps(void *ctx, uint32_t grants)
{
    struct system *sys = ctx;
    CHECK(sys->current < INIT_CAPACITY);
    struct boot_service *service = &sys->services[sys->current];
    CHECK(grants == sys->plan[sys->current].grants && !service->caps);

    uint32_t cap;
    if (cap_mint(INIT_SID, sys->root, CAP_SLOT_AUTO, (cap_rights_t)grants,
                 (cap_badge_t)(sys->current + 1), &cap) != CAP_OK)
        return 0;
    service->cap = cap;
    service->caps = resource_id(sys->current, 2);
    return service->caps;
}

static uint32_t pm_create_thread(void *ctx, uint32_t as, uint32_t caps)
{
    struct system *sys = ctx;
    struct boot_service *service = &sys->services[sys->current];
    CHECK(as == service->as && caps == service->caps && !service->thread);
    service->thread = resource_id(sys->current, 3);
    return service->thread;
}

static void pm_start_thread(void *ctx, uint32_t thread)
{
    struct system *sys = ctx;
    struct boot_service *service = &sys->services[sys->current];
    int code;

    CHECK(thread == service->thread);
    CHECK(pm_wait(&sys->pm, INIT_PID, sys->pm.last_pid, &code) == PM_RUNNING);
}

static void pm_release_resource(void *ctx, enum task_resource kind, uint32_t handle)
{
    struct system *sys = ctx;
    for (size_t i = 0; i < INIT_CAPACITY; i++) {
        struct boot_service *service = &sys->services[i];
        uint32_t *slot = kind == TASK_AS ? &service->as
                       : kind == TASK_CAPS ? &service->caps
                       : &service->thread;
        if (*slot != handle)
            continue;
        *slot = 0;
        if (kind == TASK_CAPS && service->cap) {
            CHECK(cap_delete(INIT_SID, service->cap) == CAP_OK);
            service->cap = 0;
        }
        CHECK(sys->releases < 3 * INIT_CAPACITY);
        sys->release_order[sys->releases++] = kind;
        return;
    }
    CHECK(false);
}

static enum sm_result sm_retain(void *ctx, uint32_t owner, uint32_t handle,
                                uint32_t *endpoint)
{
    struct system *sys = ctx;
    struct cap_info info;

    if (owner != INIT_OWNER || cap_info(INIT_SID, handle, &info) != CAP_OK ||
        !info.rights || !info.badge || info.badge > INIT_CAPACITY)
        return SM_DENIED;
    *endpoint = ENDPOINT_BASE + info.badge;
    sys->retains++;
    return SM_OK;
}

static enum sm_result sm_grant_send(void *ctx, uint32_t client, uint32_t endpoint,
                                    uint32_t *handle)
{
    (void)ctx;
    if (!client || endpoint <= ENDPOINT_BASE ||
        endpoint > ENDPOINT_BASE + INIT_CAPACITY)
        return SM_DENIED;
    *handle = endpoint;
    return SM_OK;
}

static void sm_release(void *ctx, uint32_t endpoint)
{
    struct system *sys = ctx;
    CHECK(endpoint > ENDPOINT_BASE && endpoint <= ENDPOINT_BASE + INIT_CAPACITY);
    CHECK(sys->retains > 0);
    sys->retains--;
}

static void system_setup(struct system *sys, const struct init_service *plan,
                         uint32_t fail_id)
{
    *sys = (struct system){.plan = plan, .fail_id = fail_id};
    cap_system_init();
    cap_oid_t oid;
    CHECK(cap_object_create(&boot_type, NULL, &oid) == CAP_OK);
    CHECK(cap_root_install(INIT_SID, 0, oid, CAP_RIGHT_ALL, 0, &sys->root) == CAP_OK);

    sys->pm_port = (struct pm_port){
        sys, pm_create_as, pm_create_caps, pm_create_thread,
        pm_start_thread, pm_release_resource,
    };
    sys->sm_port = (struct sm_port){sys, sm_retain, sm_grant_send, sm_release};
    sys->pm.port = &sys->pm_port;
    sys->sm.port = &sys->sm_port;
}

static void system_release(struct system *sys, size_t index);

static int system_start(void *ctx, const struct init_service *service,
                        struct init_child *child)
{
    struct system *sys = ctx;
    size_t index = system_index(sys, service);
    CHECK(index < INIT_CAPACITY && !sys->services[index].pid);
    if (service->id == sys->fail_id)
        return -1;

    struct boot_service *boot = &sys->services[index];
    boot->name = system_names[index];
    boot->length = text_length(boot->name);
    sys->current = (unsigned)index;

    uint64_t pid;
    if (pm_spawn(&sys->pm, INIT_PID, service->id, service->grants, &pid) != PM_OK)
        return -2;
    boot->pid = (uint32_t)pid;
    sys->live++;

    if (sm_register(&sys->sm, INIT_OWNER, boot->name, boot->length,
                    boot->cap) != SM_OK) {
        system_release(sys, index);
        return -3;
    }
    boot->registered = true;
    sys->starts++;
    *child = (struct init_child){.handle = boot->pid,
                                 .ready_receipt = RECEIPT_BASE + index};
    return 0;
}

static void system_release(struct system *sys, size_t index)
{
    struct boot_service *service = &sys->services[index];
    if (service->registered) {
        CHECK(sm_unregister(&sys->sm, INIT_OWNER, service->name,
                            service->length) == SM_OK);
        service->registered = false;
    }
    if (!service->pid)
        return;

    struct pm_process *process = NULL;
    for (size_t i = 0; i < PM_CAPACITY; i++) {
        if (sys->pm.processes[i].state == PM_LIVE &&
            sys->pm.processes[i].pid == service->pid)
            process = &sys->pm.processes[i];
    }
    CHECK(process != NULL);
    CHECK(pm_on_exit(&sys->pm, process->thread, -1));

    int code;
    CHECK(pm_wait(&sys->pm, INIT_PID, service->pid, &code) == PM_OK);
    service->pid = 0;
    CHECK(sys->live > 0);
    sys->live--;
}

static void system_stop(void *ctx, uint32_t handle)
{
    struct system *sys = ctx;
    size_t index = INIT_CAPACITY;
    for (size_t i = 0; i < INIT_CAPACITY; i++) {
        if (sys->services[i].pid == handle)
            index = i;
    }
    CHECK(index < INIT_CAPACITY);
    system_release(sys, index);
    CHECK(sys->stops < INIT_CAPACITY);
    sys->stop_order[sys->stops++] = handle;
}

static void system_boot_case(void)
{
    struct system sys;
    system_setup(&sys, system_plan, 0);
    const struct init_port port = {&sys, system_start, system_stop};

    CHECK(init_boot(&sys.init, &port, CAP_RIGHT_ALL, system_plan,
                    INIT_CAPACITY) == 0);
    CHECK(sys.init.phase == INIT_STARTING && sys.starts == 1 && sys.live == 1);
    CHECK(cap_live_caps() == 2);

    for (size_t i = 0; i < INIT_CAPACITY; i++) {
        CHECK(sys.init.started_count == i + 1);
        CHECK(init_ready(&sys.init, sys.init.started[i].ready_receipt));
    }
    CHECK(sys.init.phase == INIT_READY && sys.live == INIT_CAPACITY);

    for (size_t i = 0; i < INIT_CAPACITY; i++) {
        struct cap_info info;
        CHECK(cap_info(INIT_SID, sys.services[i].cap, &info) == CAP_OK);
        CHECK(info.rights == system_plan[i].grants && info.badge == i + 1);
        int code;
        CHECK(pm_wait(&sys.pm, INIT_PID, sys.services[i].pid, &code) == PM_RUNNING);
    }
    CHECK(sys.retains == INIT_CAPACITY);

    uint32_t handle;
    CHECK(sm_lookup(&sys.sm, CLIENT, "vfs", 3, &handle) == SM_OK &&
          handle == ENDPOINT_BASE + 4);
    CHECK(sm_lookup(&sys.sm, CLIENT, "nope", 4, &handle) == SM_NOT_FOUND);
    CHECK(cap_live_caps() == INIT_CAPACITY + 1);
    CHECK(cap_check_invariants() == CAP_OK);

    for (size_t i = INIT_CAPACITY; i > 0; i--)
        system_release(&sys, i - 1);
    CHECK(sys.live == 0 && sys.releases == 3 * INIT_CAPACITY);
    CHECK(sys.release_order[0] == TASK_THREAD &&
          sys.release_order[1] == TASK_CAPS &&
          sys.release_order[2] == TASK_AS);
    CHECK(cap_live_caps() == 1 && sys.retains == 0);
    CHECK(sm_lookup(&sys.sm, CLIENT, "sm", 2, &handle) == SM_NOT_FOUND);
    CHECK(cap_check_invariants() == CAP_OK);
    cap_system_init();
}

static void system_failure_case(void)
{
    struct system sys;
    system_setup(&sys, system_plan, 3);
    const struct init_port port = {&sys, system_start, system_stop};

    CHECK(init_boot(&sys.init, &port, CAP_RIGHT_ALL, system_plan,
                    INIT_CAPACITY) == 0);
    CHECK(init_ready(&sys.init, sys.init.started[0].ready_receipt));
    CHECK(sys.init.phase == INIT_STARTING && sys.starts == 2);
    CHECK(init_ready(&sys.init, sys.init.started[1].ready_receipt));
    CHECK(sys.init.phase == INIT_FAILED && sys.init.failed_service == 3 &&
          sys.init.error == -1);
    CHECK(sys.live == 0 && sys.stops == 2);
    CHECK(sys.stop_order[0] == 2 && sys.stop_order[1] == 1);
    CHECK(sys.releases == 6);
    for (size_t i = 0; i < 2; i++) {
        CHECK(sys.release_order[i * 3] == TASK_THREAD);
        CHECK(sys.release_order[i * 3 + 1] == TASK_CAPS);
        CHECK(sys.release_order[i * 3 + 2] == TASK_AS);
    }
    CHECK(cap_live_caps() == 1 && sys.retains == 0);

    int code;
    CHECK(pm_wait(&sys.pm, INIT_PID, 1, &code) == PM_NOT_FOUND);
    CHECK(pm_wait(&sys.pm, INIT_PID, 2, &code) == PM_NOT_FOUND);
    CHECK(cap_check_invariants() == CAP_OK);
    cap_system_init();
}

static void system_plan_case(void)
{
    const struct init_service plan[] = {{1, CAP_RIGHT_EXEC}};
    struct system sys;
    system_setup(&sys, plan, 0);
    const struct init_port port = {&sys, system_start, system_stop};

    CHECK(init_boot(&sys.init, &port, CAP_RIGHT_READ, plan, 1) == INIT_BAD_PLAN);
    CHECK(sys.init.phase == INIT_FAILED && sys.starts == 0);
    CHECK(cap_live_caps() == 1 && sys.retains == 0);
    cap_system_init();
}

static void run(void)
{
    fake_cases();
    system_boot_case();
    system_failure_case();
    system_plan_case();
    demo_log("startup: validated plans, ordered readiness and PM/SM/capability cleanup");
}

DEMO_MODULE(init, false, run, NULL);
