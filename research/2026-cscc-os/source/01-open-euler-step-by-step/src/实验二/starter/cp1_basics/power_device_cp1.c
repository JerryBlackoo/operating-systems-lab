/*
 * power_device_cp1.c - 电力设备驱动模块（阶段1：基础模块）
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ 阶段1 (CP1): 基础模块                                      │
 * │ 目标：让模块能加载卸载，并打印日志                          │
 * └─────────────────────────────────────────────────────────────┘
 *
 * TODO 任务：补全以下代码
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("电力设备模拟驱动 - 阶段1");
MODULE_VERSION("1.0");

/* TODO 1: 实现模块加载函数 power_init
 * 要求：使用 printk(KERN_INFO) 打印 "Power Device Module Loaded"
 * 返回：0
 */
static int __init power_init(void)
{
    // 在这里添加你的代码
    
    return 0;
}

/* TODO 2: 实现模块卸载函数 power_exit
 * 要求：使用 printk(KERN_INFO) 打印 "Power Device Module Unloaded"
 */
static void __exit power_exit(void)
{
    // 在这里添加你的代码
    
}

/* TODO 3: 使用 module_init() 注册加载函数 */

/* TODO 4: 使用 module_exit() 注册卸载函数 */