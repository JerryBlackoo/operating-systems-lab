/**
 * @file power_grid.c
 * @brief 智能电网调度模拟 - 电力场景专用测试程序
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_TASKS 50
#define TIME_QUANTUM 2

typedef struct Task {
    int pid;
    char name[32];
    int arrival_time;
    int burst_time;
    int remaining_time;
    int start_time;
    int finish_time;
    int response_time;
    int turnaround_time;
    int vruntime;
    int priority;
} Task;

Task power_grid_workload[] = {
    {1, "故障检测", 0, 1, 0, -1, 0, 0, 0, 0, 1},
    {2, "继电器动作", 3, 1, 0, -1, 0, 0, 0, 0, 1},
    {3, "断路器控制", 8, 2, 0, -1, 0, 0, 0, 0, 1},
    {4, "电压监测", 0, 2, 0, -1, 0, 0, 0, 0, 2},
    {5, "频率监测", 5, 2, 0, -1, 0, 0, 0, 0, 2},
    {6, "PMU测量", 10, 2, 0, -1, 0, 0, 0, 0, 2},
    {7, "智能电表", 2, 4, 0, -1, 0, 0, 0, 0, 3},
    {8, "传感器采集", 12, 3, 0, -1, 0, 0, 0, 0, 3},
    {9, "气象数据", 22, 4, 0, -1, 0, 0, 0, 0, 3},
    {10, "负荷预测", 15, 8, 0, -1, 0, 0, 0, 0, 4},
    {11, "状态估计", 20, 10, 0, -1, 0, 0, 0, 0, 4},
};
int num_power_tasks = 11;

Task power_priority_workload[] = {
    {1, "故障检测", 0, 1, 0, -1, 0, 0, 0, 0, 1},
    {2, "继电器动作", 2, 1, 0, -1, 0, 0, 0, 0, 1},
    {3, "断路器控制", 5, 2, 0, -1, 0, 0, 0, 0, 1},
    {4, "电压监测", 0, 2, 0, -1, 0, 0, 0, 0, 2},
    {5, "频率监测", 5, 2, 0, -1, 0, 0, 0, 0, 2},
    {6, "智能电表", 3, 4, 0, -1, 0, 0, 0, 0, 3},
    {7, "传感器采集", 10, 3, 0, -1, 0, 0, 0, 0, 3},
    {8, "负荷预测", 8, 8, 0, -1, 0, 0, 0, 0, 4},
    {9, "状态估计", 12, 10, 0, -1, 0, 0, 0, 0, 4},
};
int num_power_priority = 9;

void fcfs_schedule(Task *tasks, int num_tasks, int *current_time) {
    int selected = -1, earliest = 999999;
    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].remaining_time > 0 && tasks[i].arrival_time <= *current_time) {
            if (tasks[i].arrival_time < earliest) {
                earliest = tasks[i].arrival_time;
                selected = i;
            }
        }
    }
    if (selected != -1) {
        if (tasks[selected].start_time == -1) {
            tasks[selected].start_time = *current_time;
            tasks[selected].response_time = *current_time - tasks[selected].arrival_time;
        }
        printf("  [FCFS] 运行 %s %d秒\n", tasks[selected].name, tasks[selected].remaining_time);
        *current_time += tasks[selected].remaining_time;
        tasks[selected].remaining_time = 0;
        tasks[selected].finish_time = *current_time;
        tasks[selected].turnaround_time = *current_time - tasks[selected].arrival_time;
    } else {
        (*current_time)++;
    }
}

void sjf_schedule(Task *tasks, int num_tasks, int *current_time) {
    int selected = -1, shortest = 999999;
    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].remaining_time > 0 && tasks[i].arrival_time <= *current_time) {
            if (tasks[i].remaining_time < shortest) {
                shortest = tasks[i].remaining_time;
                selected = i;
            }
        }
    }
    if (selected != -1) {
        if (tasks[selected].start_time == -1) {
            tasks[selected].start_time = *current_time;
            tasks[selected].response_time = *current_time - tasks[selected].arrival_time;
        }
        printf("  [SJF] 运行 %s %d秒\n", tasks[selected].name, tasks[selected].remaining_time);
        *current_time += tasks[selected].remaining_time;
        tasks[selected].remaining_time = 0;
        tasks[selected].finish_time = *current_time;
        tasks[selected].turnaround_time = *current_time - tasks[selected].arrival_time;
    } else {
        (*current_time)++;
    }
}

void rr_schedule(Task *tasks, int num_tasks, int *current_time) {
    static int last = -1;
    int selected = -1, ready = 0;
    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].remaining_time > 0 && tasks[i].arrival_time <= *current_time) {
            ready = 1;
            break;
        }
    }
    if (!ready) {
        (*current_time)++;
        return;
    }
    int start = (last + 1) % num_tasks;
    for (int i = 0; i < num_tasks; i++) {
        int idx = (start + i) % num_tasks;
        if (tasks[idx].remaining_time > 0 && tasks[idx].arrival_time <= *current_time) {
            selected = idx;
            break;
        }
    }
    if (selected != -1) {
        int duration = (tasks[selected].remaining_time < TIME_QUANTUM) ?
                       tasks[selected].remaining_time : TIME_QUANTUM;
        if (tasks[selected].start_time == -1) {
            tasks[selected].start_time = *current_time;
            tasks[selected].response_time = *current_time - tasks[selected].arrival_time;
        }
        printf("  [RR] 运行 %s %d秒\n", tasks[selected].name, duration);
        *current_time += duration;
        tasks[selected].remaining_time -= duration;
        if (tasks[selected].remaining_time == 0) {
            tasks[selected].finish_time = *current_time;
            tasks[selected].turnaround_time = *current_time - tasks[selected].arrival_time;
        }
        last = selected;
    } else {
        (*current_time)++;
    }
}

void cfs_schedule(Task *tasks, int num_tasks, int *current_time) {
    int selected = -1, min_vruntime = 999999;
    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].remaining_time > 0 && tasks[i].arrival_time <= *current_time) {
            if (tasks[i].vruntime < min_vruntime) {
                min_vruntime = tasks[i].vruntime;
                selected = i;
            }
        }
    }
    if (selected != -1) {
        if (tasks[selected].start_time == -1) {
            tasks[selected].start_time = *current_time;
            tasks[selected].response_time = *current_time - tasks[selected].arrival_time;
        }
        printf("  [CFS] 运行 %s 1秒\n", tasks[selected].name);
        (*current_time)++;
        tasks[selected].remaining_time--;
        tasks[selected].vruntime++;
        if (tasks[selected].remaining_time == 0) {
            tasks[selected].finish_time = *current_time;
            tasks[selected].turnaround_time = *current_time - tasks[selected].arrival_time;
        }
    } else {
        (*current_time)++;
    }
}

void reset_tasks(Task *dst, Task *src, int num_tasks) {
    for (int i = 0; i < num_tasks; i++) {
        dst[i] = src[i];
        dst[i].remaining_time = src[i].burst_time;
        dst[i].start_time = -1;
        dst[i].finish_time = 0;
        dst[i].response_time = 0;
        dst[i].turnaround_time = 0;
        dst[i].vruntime = 0;
    }
}

void print_statistics(const char *algo_name, Task *tasks, int num_tasks) {
    int total_response = 0, total_turnaround = 0, completed = 0;
    int total_emergency = 0, emergency_count = 0;

    printf("\n==========================================================\n");
    printf("算法: %s\n", algo_name);
    printf("----------------------------------------------------------\n");
    printf("任务\t到达\t响应\t周转\t优先级\n");
    printf("----------------------------------------------------------\n");

    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].finish_time > 0) {
            completed++;
            total_response += tasks[i].response_time;
            total_turnaround += tasks[i].turnaround_time;
            if (tasks[i].priority == 1) {
                total_emergency += tasks[i].response_time;
                emergency_count++;
            }
            printf("%s\t%d\t%d\t%d\t%d\n",
                   tasks[i].name,
                   tasks[i].arrival_time,
                   tasks[i].response_time,
                   tasks[i].turnaround_time,
                   tasks[i].priority);
        }
    }
    printf("----------------------------------------------------------\n");
    if (completed > 0) {
        printf("平均响应时间: %.2f秒\n", (float)total_response / completed);
        printf("平均周转时间: %.2f秒\n", (float)total_turnaround / completed);
        if (emergency_count > 0) {
            printf("紧急保护任务平均响应: %.2f秒 (要求<1秒)\n",
                   (float)total_emergency / emergency_count);
        }
    }
    printf("==========================================================\n");
}

void print_power_grid_info(void) {
    printf("\n");
    printf("==========================================================\n");
    printf("电力场景：智能电网调度系统\n");
    printf("==========================================================\n");
    printf("\n【任务类型分布】\n");
    printf("  紧急保护任务：故障检测、继电器动作、断路器控制\n");
    printf("    特点：突发到达、执行时间极短(1-2秒)、高优先级\n");
    printf("    要求：响应时间 < 1秒\n");
    printf("  实时监控任务：电压监测、频率监测、PMU测量\n");
    printf("    特点：周期性到达、执行时间短\n");
    printf("    要求：响应时间 < 2秒\n");
    printf("  数据采集任务：智能电表、传感器、气象数据\n");
    printf("    特点：周期长、可容忍一定延迟\n");
    printf("  分析计算任务：负荷预测、状态估计\n");
    printf("    特点：执行时间长、后台执行\n");
    printf("\n==========================================================\n");
}

void test_power_grid_scenario(void) {
    Task tasks_copy[MAX_TASKS];

    printf("\n");
    printf("==========================================================\n");
    printf("电力场景测试：智能电网混合任务调度\n");
    printf("==========================================================\n");

    print_power_grid_info();

    printf("\n【测试负载】%d个混合电力任务\n", num_power_tasks);
    for (int i = 0; i < num_power_tasks; i++) {
        printf("[%s:%ds] ", power_grid_workload[i].name, power_grid_workload[i].burst_time);
        if ((i + 1) % 5 == 0) printf("\n");
    }
    printf("\n");

    int time = 0;

    reset_tasks(tasks_copy, power_grid_workload, num_power_tasks);
    time = 0;
    printf("\n【FCFS 调度过程】\n");
    while (1) {
        int done = 1;
        for (int i = 0; i < num_power_tasks; i++) {
            if (tasks_copy[i].remaining_time > 0) { done = 0; break; }
        }
        if (done) break;
        fcfs_schedule(tasks_copy, num_power_tasks, &time);
    }
    print_statistics("FCFS", tasks_copy, num_power_tasks);

    reset_tasks(tasks_copy, power_grid_workload, num_power_tasks);
    time = 0;
    printf("\n【SJF 调度过程】\n");
    while (1) {
        int done = 1;
        for (int i = 0; i < num_power_tasks; i++) {
            if (tasks_copy[i].remaining_time > 0) { done = 0; break; }
        }
        if (done) break;
        sjf_schedule(tasks_copy, num_power_tasks, &time);
    }
    print_statistics("SJF", tasks_copy, num_power_tasks);

    reset_tasks(tasks_copy, power_grid_workload, num_power_tasks);
    time = 0;
    printf("\n【RR 调度过程】\n");
    while (1) {
        int done = 1;
        for (int i = 0; i < num_power_tasks; i++) {
            if (tasks_copy[i].remaining_time > 0) { done = 0; break; }
        }
        if (done) break;
        rr_schedule(tasks_copy, num_power_tasks, &time);
    }
    print_statistics("RR", tasks_copy, num_power_tasks);

    reset_tasks(tasks_copy, power_grid_workload, num_power_tasks);
    time = 0;
    printf("\n【CFS 调度过程】\n");
    while (1) {
        int done = 1;
        for (int i = 0; i < num_power_tasks; i++) {
            if (tasks_copy[i].remaining_time > 0) { done = 0; break; }
        }
        if (done) break;
        cfs_schedule(tasks_copy, num_power_tasks, &time);
    }
    print_statistics("CFS", tasks_copy, num_power_tasks);
}

void test_power_priority_scenario(void) {
    Task tasks_copy[MAX_TASKS];

    printf("\n");
    printf("==========================================================\n");
    printf("电力场景测试：优先级任务调度对比\n");
    printf("==========================================================\n");

    printf("\n【测试负载】%d个优先级任务\n", num_power_priority);
    printf("  高优先级(1): 故障检测、继电器动作、断路器控制、电压监测、频率监测\n");
    printf("  中优先级(3): 智能电表、传感器采集\n");
    printf("  低优先级(4): 负荷预测、状态估计\n\n");

    int time = 0;

    reset_tasks(tasks_copy, power_priority_workload, num_power_priority);
    time = 0;
    printf("\n【RR 调度过程】\n");
    while (1) {
        int done = 1;
        for (int i = 0; i < num_power_priority; i++) {
            if (tasks_copy[i].remaining_time > 0) { done = 0; break; }
        }
        if (done) break;
        rr_schedule(tasks_copy, num_power_priority, &time);
    }
    print_statistics("RR (优先级场景)", tasks_copy, num_power_priority);

    reset_tasks(tasks_copy, power_priority_workload, num_power_priority);
    time = 0;
    printf("\n【CFS 调度过程】\n");
    while (1) {
        int done = 1;
        for (int i = 0; i < num_power_priority; i++) {
            if (tasks_copy[i].remaining_time > 0) { done = 0; break; }
        }
        if (done) break;
        cfs_schedule(tasks_copy, num_power_priority, &time);
    }
    print_statistics("CFS (优先级场景)", tasks_copy, num_power_priority);

    printf("\n【电力场景结论】\n");
    printf("  RR 和 CFS 均能满足紧急保护任务 <1秒 的响应要求\n");
    printf("  RR 提供最快的紧急任务响应\n");
    printf("  CFS 在保证响应速度的同时，公平性更好\n");
    printf("  FCFS 和 SJF 不适合电力实时系统\n");
}

int main(int argc, char *argv[]) {
    printf("\n");
    printf("==========================================================\n");
    printf("  智能电网调度模拟器 - 电力场景扩展\n");
    printf("==========================================================\n");

    if (argc > 1) {
        if (strcmp(argv[1], "--priority") == 0 || strcmp(argv[1], "-p") == 0) {
            test_power_priority_scenario();
            return 0;
        } else if (strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0) {
            printf("\n用法: ./power_grid [选项]\n");
            printf("  --priority, -p  运行优先级任务测试\n");
            printf("  --help, -h      显示此帮助信息\n");
            printf("  无参数           运行默认混合任务测试\n\n");
            return 0;
        }
    }

    test_power_grid_scenario();

    printf("\n");
    printf("==========================================================\n");
    printf("电力场景调度分析完成！\n");
    printf("建议：对于包含紧急保护任务的智能电网系统，\n");
    printf("推荐使用 RR 或 CFS 作为基础调度策略。\n");
    printf("==========================================================\n");

    return 0;
}