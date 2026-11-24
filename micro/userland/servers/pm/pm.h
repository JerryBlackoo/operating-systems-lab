#ifndef MICRO_MODEL_PM_H
#define MICRO_MODEL_PM_H

#include <stdbool.h>
#include <stdint.h>

/*
 * 用户态进程管理的最小模型。
 *
 *   Process = (pid, parent, address_space, thread, cap_space, state, exit_code)
 *   PM      = (process_table, next_pid)
 *
 *   EMPTY --spawn 成功--> LIVE --确认退出--> ZOMBIE --父进程 wait--> EMPTY
 *
 * PM 只组合三类执行资源，不实现调度、页表和能力语义。as/caps/thread 都是
 * 来自 pm_port 的不透明句柄，分别由 memory/vm、capability 和 sched 提供。
 */

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

/* 清空进程表并绑定资源提供者；调用前不得使用 pm。 */
void pm_init(struct pm *, const struct pm_port *);

/*
 * 建立三个资源后发布 LIVE 记录和 PID，最后才放行线程。
 * 任一资源创建失败时按逆序交还本次取得的资源，不消费 PID 和槽位。
 */
enum pm_result pm_spawn(struct pm *, uint64_t parent, uint32_t image, uint32_t grants, uint64_t *pid);

/* 用线程身份定位 LIVE 记录，交还执行资源并保留退出码；重复确认返回 false。 */
bool pm_on_exit(struct pm *, uint32_t thread, int exit_code);

/* 核对父子关系；LIVE 返回 PM_RUNNING，ZOMBIE 交付退出码并释放槽位。 */
enum pm_result pm_wait(struct pm *, uint64_t parent, uint64_t pid, int *exit_code);

#endif
