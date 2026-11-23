#ifndef MICRO_MODEL_PM_H
#define MICRO_MODEL_PM_H

#include <stdbool.h>
#include <stdint.h>

enum pm_result { PM_OK, PM_RUNNING, PM_BAD_ARG, PM_NO_SPACE, PM_NOT_FOUND, PM_DENIED };
enum pm_state { PM_EMPTY, PM_LIVE, PM_ZOMBIE };
enum task_resource { TASK_AS, TASK_CAPS, TASK_THREAD };
enum { PM_CAPACITY = 4 };

struct pm_port {
    void *ctx;
    uint32_t (*create_as)(void *, uint32_t image);
    uint32_t (*create_caps)(void *, uint32_t grants);
    uint32_t (*create_thread)(void *, uint32_t as, uint32_t caps);
    void (*start)(void *, uint32_t thread);
    void (*release)(void *, enum task_resource, uint32_t handle);
};

struct pm_process {
    enum pm_state state;
    uint64_t pid, parent;
    uint32_t as, caps, thread;
    int exit_code;
};

struct pm {
    const struct pm_port *port;
    uint64_t last_pid;
    struct pm_process processes[PM_CAPACITY];
};

enum pm_result pm_spawn(struct pm *, uint64_t parent, uint32_t image, uint32_t grants, uint64_t *pid);
bool pm_on_exit(struct pm *, uint32_t thread, int exit_code);
enum pm_result pm_wait(struct pm *, uint64_t parent, uint64_t pid, int *exit_code);

#endif
