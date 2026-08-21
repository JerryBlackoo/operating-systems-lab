#include "scheduler.h"

/*
 * ============================================================
 * TODO 2：实现 SJF（短作业优先）调度算法
 * ============================================================
 * 
 * 功能：在就绪队列中选择剩余执行时间最短的任务执行
 * 
 * 步骤：
 *   1. 遍历所有任务，找出满足以下条件的任务：
 *      - remaining_time > 0（尚未完成）
 *      - arrival_time <= current_time（已到达）
 *   2. 在符合条件的任务中，选择 remaining_time 最小的任务
 *   3. 如果找到，调用 run_task(selected, tasks[selected].remaining_time)
 *   4. 如果没找到，CPU 空闲等待下一个任务到达
 * 
 * 提示：
 *   - 使用 999999 作为初始比较值
 *   - 与 FCFS 的区别在于选择标准不同
 * ============================================================
 */

void sjf_schedule(Task *tasks, int num_tasks, int current_time) {
    /* TODO 2：实现 SJF 调度算法 */
    
    int selected = -1;
    int shortest_burst = 999999;
    
    // TODO 2.1：遍历所有任务，选择剩余时间最短的已就绪任务
    for (int i = 0; i < num_tasks; i++) {
        // 在这里添加你的代码
        
    }
    
    if (selected != -1) {
        // TODO 2.2：执行选中的任务
        
    } else {
        // TODO 2.3：CPU 空闲等待
        
    }
}