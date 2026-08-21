#include "scheduler.h"

#define TIME_QUANTUM 2

void rr_schedule(Task *tasks, int num_tasks, int current_time) {
    static int last_selected = -1;
    int selected = -1;
    
    int start_idx = (last_selected + 1) % num_tasks;
    
    for (int i = 0; i < num_tasks; i++) {
        int idx = (start_idx + i) % num_tasks;
        if (tasks[idx].remaining_time > 0 && tasks[idx].arrival_time <= current_time) {
            selected = idx;
            break;
        }
    }
    
    if (selected != -1) {
        int run_duration = (tasks[selected].remaining_time < TIME_QUANTUM) ? 
                           tasks[selected].remaining_time : TIME_QUANTUM;
        run_task(selected, run_duration);
        last_selected = selected;
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