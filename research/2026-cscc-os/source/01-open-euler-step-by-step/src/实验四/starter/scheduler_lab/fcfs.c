#include "scheduler.h"

/*
 * ============================================================
 * TODO 1：实现 FCFS（先来先服务）调度算法
 * ============================================================
 * 
 * 功能：在就绪队列中选择最早到达的任务执行
 * 
 * 步骤：
 *   1. 遍历所有任务，找出满足以下条件的任务：
 *      - remaining_time > 0（尚未完成）
 *      - arrival_time <= current_time（已到达）
 *   2. 在符合条件的任务中，选择 arrival_time 最小的任务
 *   3. 如果找到，调用 run_task(selected, tasks[selected].remaining_time)
 *   4. 如果没找到，表示 CPU 空闲，等待下一个任务到达：
 *      - 遍历所有未完成的任务，找出最小的 arrival_time
 *      - 将 g_current_time 设置为 next_arrival
 *      - 打印空闲信息
 * 
 * 提示：
 *   - 使用 999999 作为初始比较值（代表无穷大）
 *   - 注意：selected 初始化为 -1
 * ============================================================
 */

void fcfs_schedule(Task *tasks, int num_tasks, int current_time) {
    /* TODO 1：实现 FCFS 调度算法 */
    
    int selected = -1;
    int earliest_arrival = 999999;
    
    // TODO 1.1：遍历所有任务，选择最早到达的已就绪任务
    for (int i = 0; i < num_tasks; i++) {
        // 在这里添加你的代码
        
    }
    
    if (selected != -1) {
        // TODO 1.2：执行选中的任务（运行剩余时间）
        
    } else {
        // TODO 1.3：没有可执行任务，CPU 空闲等待
        
    }
}