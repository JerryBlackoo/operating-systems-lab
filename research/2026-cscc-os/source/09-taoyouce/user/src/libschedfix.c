#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <sys/syscall.h>
#include <unistd.h>

static int wrap_sched_getscheduler(pid_t pid) {
    long ret = syscall(SYS_sched_getscheduler, pid);
    if (ret == -1) {
        return -1;
    }
    return (int)ret;
}

static int wrap_sched_getparam(pid_t pid, struct sched_param *param) {
    long ret = syscall(SYS_sched_getparam, pid, param);
    if (ret == -1) {
        return -1;
    }
    return 0;
}

static int wrap_sched_setparam(pid_t pid, const struct sched_param *param) {
    long ret = syscall(SYS_sched_setparam, pid, param);
    if (ret == -1) {
        return -1;
    }
    return 0;
}

static int wrap_sched_setscheduler(pid_t pid, int policy, const struct sched_param *param) {
    long ret = syscall(SYS_sched_setscheduler, pid, policy, param);
    if (ret == -1) {
        return -1;
    }
    return 0;
}

static int wrap_sched_rr_get_interval(pid_t pid, struct timespec *tp) {
    long ret = syscall(SYS_sched_rr_get_interval, pid, tp);
    if (ret == -1) {
        return -1;
    }
    return 0;
}

static int wrap_sched_getaffinity(pid_t pid, size_t cpusetsize, cpu_set_t *mask) {
    long ret = syscall(SYS_sched_getaffinity, pid, cpusetsize, mask);
    if (ret == -1) {
        return -1;
    }
    return 0;
}

static int wrap_sched_setaffinity(pid_t pid, size_t cpusetsize, const cpu_set_t *mask) {
    long ret = syscall(SYS_sched_setaffinity, pid, cpusetsize, mask);
    if (ret == -1) {
        return -1;
    }
    return 0;
}

int sched_getscheduler(pid_t pid) { return wrap_sched_getscheduler(pid); }
int sched_getparam(pid_t pid, struct sched_param *param) { return wrap_sched_getparam(pid, param); }
int sched_setparam(pid_t pid, const struct sched_param *param) { return wrap_sched_setparam(pid, param); }
int sched_setscheduler(pid_t pid, int policy, const struct sched_param *param) { return wrap_sched_setscheduler(pid, policy, param); }
int sched_rr_get_interval(pid_t pid, struct timespec *tp) { return wrap_sched_rr_get_interval(pid, tp); }
int sched_getaffinity(pid_t pid, size_t cpusetsize, cpu_set_t *mask) { return wrap_sched_getaffinity(pid, cpusetsize, mask); }
int sched_setaffinity(pid_t pid, size_t cpusetsize, const cpu_set_t *mask) { return wrap_sched_setaffinity(pid, cpusetsize, mask); }
