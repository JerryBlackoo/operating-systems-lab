#include "scheduler.h"

void cfs_schedule(Task *tasks, int num_tasks, int current_time) {
    int selected = -1;
    int min_vruntime = 999999;
    
    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].remaining_time > 0 && tasks[i].arrival_time <= current_time) {
            if (tasks[i].vruntime < min_vruntime) {
                min_vruntime = tasks[i].vruntime;
                selected = i;
            }
        }
    }
    
    if (selected != -1) {
        int run_duration = 1;
        if (tasks[selected].remaining_time < run_duration) {
            run_duration = tasks[selected].remaining_time;
        }
        run_task(selected, run_duration);
        tasks[selected].vruntime += run_duration;
    } else {
        int next_arrival = 999999;
        for (int i = 0; i < num_tasks; i++) {
            if (tasks[i].remaining_time > 0 && tasks[i].arrival_time < next_arrival) {
                next_arrival = tasks[i].arrival_time;
            }
        }
        if (next_arrival != 999999) {
            printf("  [时间 %d] CPU空闲，等待下一个任务于 %d 到达\n", current_time, next_arrival);
            g_current_time = next_arrival;
        }
    }
}