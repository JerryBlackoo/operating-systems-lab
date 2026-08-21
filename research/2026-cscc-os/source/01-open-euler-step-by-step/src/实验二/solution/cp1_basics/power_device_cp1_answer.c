/*
 * power_device_cp1_answer.c - 电力设备驱动模块（阶段1完整答案）
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ 阶段1 (CP1): 基础模块 - 完整答案                          │
 * └─────────────────────────────────────────────────────────────┘
 *
 * 功能：模块加载时打印日志，卸载时打印日志
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("每天好懵逼地活着");
MODULE_DESCRIPTION("电力设备模拟驱动 - 阶段1");
MODULE_VERSION("1.0");

/* TODO 1 答案: 实现模块加载函数 */
static int __init power_init(void)
{
    printk(KERN_INFO "Power Device Module Loaded\n");
    return 0;
}

/* TODO 2 答案: 实现模块卸载函数 */
static void __exit power_exit(void)
{
    printk(KERN_INFO "Power Device Module Unloaded\n");
}

/* TODO 3 答案: 使用 module_init() 注册加载函数 */
module_init(power_init);

/* TODO 4 答案: 使用 module_exit() 注册卸载函数 */
module_exit(power_exit);
