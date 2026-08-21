#include "scheduler.h"

/*
 * ============================================================
 * TODO 4：实现 CFS（完全公平调度）调度算法
 * ============================================================
 * 
 * 功能：选择 vruntime（虚拟运行时间）最小的任务执行
 * 
 * 步骤：
 *   1. 遍历所有任务，找出满足以下条件的任务：
 *      - remaining_time > 0（尚未完成）
 *      - arrival_time <= current_time（已到达）
 *   2. 在符合条件的任务中，选择 vruntime 最小的任务
 *   3. 如果找到，执行 1 个时间单位（或剩余时间）
 *   4. 执行后，更新该任务的 vruntime += run_duration
 *   5. 如果没找到，CPU 空闲等待
 * 
 * 提示：
 *   - 每个任务初始 vruntime = 0
 *   - 运行 1 秒后，vruntime 增加 1
 *   - CFS 始终选择 vruntime 最小的任务，实现公平调度
 * ============================================================
 */

void cfs_schedule(Task *tasks, int num_tasks, int current_time) {
    /* TODO 4：实现 CFS 调度算法 */
    
    int selected = -1;
    int min_vruntime = 999999;
    
    // TODO 4.1：遍历所有任务，选择 vruntime 最小的已就绪任务
    for (int i = 0; i < num_tasks; i++) {
        // 在这里添加你的代码
        
    }
    
    if (selected != -1) {
        // TODO 4.2：执行选中的任务（运行 1 秒）
        int run_duration = 1;
        // 如果剩余时间不足 1 秒，则运行剩余时间
        
        // TODO 4.3：更新 vruntime
        
    } else {
        // TODO 4.4：CPU 空闲等待
        
    }
}