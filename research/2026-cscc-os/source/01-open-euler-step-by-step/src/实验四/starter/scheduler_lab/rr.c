#include "scheduler.h"

/*
 * ============================================================
 * TODO 3：实现 RR（时间片轮转）调度算法
 * ============================================================
 * 
 * 功能：按固定时间片（TIME_QUANTUM）轮流执行就绪任务
 * 
 * 步骤：
 *   1. 从上次选中的任务位置开始，轮转查找下一个就绪任务
 *   2. 遍历所有任务，找出满足以下条件的任务：
 *      - remaining_time > 0（尚未完成）
 *      - arrival_time <= current_time（已到达）
 *   3. 从 (last_selected + 1) % num_tasks 开始查找
 *   4. 如果找到，执行 min(remaining_time, TIME_QUANTUM)
 *   5. 更新 last_selected = selected
 *   6. 如果没找到，CPU 空闲等待
 * 
 * 提示：
 *   - TIME_QUANTUM 已在文件开头定义
 *   - 使用静态变量 last_selected 记录上次选中的任务
 *   - 查找时用 (start_idx + i) % num_tasks 实现循环
 * ============================================================
 */

#define TIME_QUANTUM 2

void rr_schedule(Task *tasks, int num_tasks, int current_time) {
    /* TODO 3：实现 RR 调度算法 */
    
    static int last_selected = -1;
    int selected = -1;
    
    // TODO 3.1：从上次位置的下一个开始循环查找
    int start_idx = (last_selected + 1) % num_tasks;
    
    for (int i = 0; i < num_tasks; i++) {
        int idx = (start_idx + i) % num_tasks;
        // 在这里添加你的代码
        
    }
    
    if (selected != -1) {
        // TODO 3.2：执行选中的任务（运行一个时间片）
        int run_duration = 0;
        // 计算运行时间：min(remaining_time, TIME_QUANTUM)
        
    } else {
        // TODO 3.3：CPU 空闲等待
        
    }
}