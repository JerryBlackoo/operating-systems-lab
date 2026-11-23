#ifndef MICRO_MODEL_DEMO_H
#define MICRO_MODEL_DEMO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

struct trap_frame;

void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *dst, int byte, size_t n);
int memcmp(const void *left, const void *right, size_t n);

struct demo_module {
    const char *name;
    bool user;
    void (*run)(void);
    struct trap_frame *(*trap)(struct trap_frame *);
};

#define DEMO_MODULE(id, in_user, run_fn, trap_fn) \
    const struct demo_module demo_##id = { \
        .name = #id, .user = (in_user), .run = (run_fn), .trap = (trap_fn), \
    }

void demo_run(const struct demo_module *module);
void demo_log(const char *line);
_Noreturn void demo_pass(void);
_Noreturn void demo_fail(const char *condition, unsigned line);
_Noreturn void demo_enter_user(void (*entry)(uintptr_t), uintptr_t argument);

#define CHECK(condition) do { if (!(condition)) demo_fail(#condition, __LINE__); } while (0)

#endif
