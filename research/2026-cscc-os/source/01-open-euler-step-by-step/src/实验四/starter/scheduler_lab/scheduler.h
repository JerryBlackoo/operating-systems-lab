#ifndef SCHEDULER_H
#define SCHEDULER_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct Task {
    int pid;
    int arrival_time;
    int burst_time;
    int remaining_time;
    int start_time;
    int finish_time;
    int response_time;
    int turnaround_time;
    int vruntime;
} Task;

typedef void (*sched_func_t)(Task *tasks, int num_tasks, int current_time);

extern Task *g_tasks;
extern int g_num_tasks;
extern int g_current_time;

void fcfs_schedule(Task *tasks, int num_tasks, int current_time);
void sjf_schedule(Task *tasks, int num_tasks, int current_time);
void rr_schedule(Task *tasks, int num_tasks, int current_time);
void cfs_schedule(Task *tasks, int num_tasks, int current_time);

void run_task(int task_index, int duration);
int all_tasks_finished(Task *tasks, int num_tasks);
void print_statistics(char *alg_name);
void simulate(sched_func_t schedule, char *alg_name, Task *tasks, int num_tasks);

#endif