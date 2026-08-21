/*
 * power_device_cp2.c - 电力设备驱动模块（阶段2：添加模块参数）
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ 阶段2 (CP2): 添加模块参数                                 │
 * │ 目标：支持 sensor_name 和 default_voltage 参数            │
 * └─────────────────────────────────────────────────────────────┘
 *
 * TODO 任务：补全以下代码
 *
 * 命名提示：
 *   · 不要用 dev_name 作为变量名 —— <linux/device.h> 里已经有同名 inline 函数，
 *     在 openEuler 6.6 内核上会直接报 "‘dev_name’被重新声明为不同意义的符号" 错误。
 *   · 请用 sensor_name 代替。
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("电力设备模拟驱动 - 阶段2");
MODULE_VERSION("1.0");

/* TODO 5: 声明模块参数
 * 要求：
 *   - 字符串参数 sensor_name，默认值 "power_sensor"
 *   - 整型参数 default_voltage，默认值 220
 *   - 使用 module_param() 和 MODULE_PARM_DESC() 注册
 */

/* TODO 1: 实现模块加载函数 power_init
 * 要求：打印 "Power Device Module Loaded" 和参数值
 */
static int __init power_init(void)
{
    printk(KERN_INFO "Power Device Module Loaded\n");
    
    /* TODO 6: 打印模块参数值
     * 格式："Sensor name: %s, default voltage: %d V\n"
     */
    
    return 0;
}

/* TODO 2: 实现模块卸载函数 power_exit */
static void __exit power_exit(void)
{
    printk(KERN_INFO "Power Device Module Unloaded\n");
}

/* TODO 3: 使用 module_init() 注册加载函数 */

/* TODO 4: 使用 module_exit() 注册卸载函数 */
