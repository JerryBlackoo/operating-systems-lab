#include "scheduler.h"

Task *g_tasks;
int g_num_tasks;
int g_current_time = 0;

static int total_turnaround_time = 0;
static int total_response_time = 0;

// ==================== 任务负载集 ====================
Task mixed_workload[] = {
    {1, 0, 8, 0, -1, 0, 0, 0, 0},
    {2, 1, 4, 0, -1, 0, 0, 0, 0},
    {3, 2, 6, 0, -1, 0, 0, 0, 0},
    {4, 3, 2, 0, -1, 0, 0, 0, 0},
    {5, 4, 5, 0, -1, 0, 0, 0, 0},
};
int num_mixed = 5;  // 数组大小

// ==================== 运行任务 ====================
void run_task(int task_index, int duration) {
    Task *task = &g_tasks[task_index];
    
    if (task->start_time == -1) {
        task->start_time = g_current_time;
        task->response_time = task->start_time - task->arrival_time;
        total_response_time += task->response_time;
    }
    
    printf("  [时间 %d] 运行任务 %d，持续 %d 秒\n", g_current_time, task->pid, duration);
    
    task->remaining_time -= duration;
    g_current_time += duration;
    
    if (task->remaining_time == 0) {
        task->finish_time = g_current_time;
        task->turnaround_time = task->finish_time - task->arrival_time;
        total_turnaround_time += task->turnaround_time;
        printf("  [时间 %d] 任务 %d 完成！周转时间=%d\n", g_current_time, task->pid, task->turnaround_time);
    }
}

// ==================== 检查是否全部完成 ====================
int all_tasks_finished(Task *tasks, int num_tasks) {
    for (int i = 0; i < num_tasks; i++) {
        if (tasks[i].remaining_time > 0) {
            return 0;
        }
    }
    return 1;
}

// ==================== 打印统计结果并导出CSV ====================
void print_statistics(char *alg_name) {
    printf("\n========== %s 调度结果 ==========\n", alg_name);
    printf("任务ID\t到达时间\t执行时间\t开始时间\t完成时间\t响应时间\t周转时间\n");
    
    for (int i = 0; i < g_num_tasks; i++) {
        Task t = g_tasks[i];
        printf("%d\t%d\t\t%d\t\t%d\t\t%d\t\t%d\t\t%d\n", 
               t.pid, t.arrival_time, t.burst_time, t.start_time, 
               t.finish_time, t.response_time, t.turnaround_time);
    }
    
    float avg_turnaround = (float)total_turnaround_time / g_num_tasks;
    float avg_response = (float)total_response_time / g_num_tasks;
    printf("平均周转时间: %.2f\n", avg_turnaround);
    printf("平均响应时间: %.2f\n", avg_response);
    
    char filename[50];
    sprintf(filename, "%s_results.csv", alg_name);
    FILE *fp = fopen(filename, "w");
    fprintf(fp, "算法,平均周转时间,平均响应时间\n");
    fprintf(fp, "%s,%.2f,%.2f\n", alg_name, avg_turnaround, avg_response);
    fclose(fp);
}

// ==================== 核心模拟函数 ====================
void simulate(sched_func_t schedule, char *alg_name, Task *tasks, int num_tasks) {
    g_tasks = tasks;
    g_num_tasks = num_tasks;
    g_current_time = 0;
    total_turnaround_time = 0;
    total_response_time = 0;
    
    for (int i = 0; i < num_tasks; i++) {
        tasks[i].remaining_time = tasks[i].burst_time;
        tasks[i].start_time = -1;
        tasks[i].finish_time = -1;
        tasks[i].vruntime = 0;
    }
    
    printf("\n开始模拟 %s 调度算法...\n", alg_name);
    
    while (!all_tasks_finished(tasks, num_tasks)) {
        schedule(tasks, num_tasks, g_current_time);
    }
    
    print_statistics(alg_name);
}

// ==================== 主函数 ====================
int main() {
    Task tasks_copy[10];
    
    printf("========================================\n");
    printf("   调度算法性能对比测试平台\n");
    printf("========================================\n");
    printf("\n【测试负载集：混合型任务（%d个任务）】\n", num_mixed);
    
    // 测试 FCFS
    memcpy(tasks_copy, mixed_workload, sizeof(mixed_workload));
    simulate(fcfs_schedule, "FCFS", tasks_copy, num_mixed);
    
    // 测试 SJF
    memcpy(tasks_copy, mixed_workload, sizeof(mixed_workload));
    simulate(sjf_schedule, "SJF", tasks_copy, num_mixed);
    
    // 测试 RR
    memcpy(tasks_copy, mixed_workload, sizeof(mixed_workload));
    simulate(rr_schedule, "RR", tasks_copy, num_mixed);
    
    // 测试 CFS
    memcpy(tasks_copy, mixed_workload, sizeof(mixed_workload));
    simulate(cfs_schedule, "CFS", tasks_copy, num_mixed);
    
    printf("\n========================================\n");
    printf("   所有调度算法测试完成！\n");
    printf("   生成的CSV文件：FCFS_results.csv, SJF_results.csv, RR_results.csv, CFS_results.csv\n");
    printf("========================================\n");
    
    return 0;
}