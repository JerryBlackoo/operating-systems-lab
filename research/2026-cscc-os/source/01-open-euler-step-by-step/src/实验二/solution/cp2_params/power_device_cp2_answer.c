/*
 * power_device_cp2_answer.c - 电力设备驱动模块（阶段2完整答案）
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ 阶段2 (CP2): 添加模块参数 - 完整答案                       │
 * └─────────────────────────────────────────────────────────────┘
 *
 * 功能：在阶段1基础上添加模块参数 sensor_name 和 default_voltage
 * 注意：变量名不能取 dev_name —— 它在内核 <linux/device.h> 中已经是
 *       static inline const char *dev_name(const struct device *dev)，
 *       直接重定义会编译不过。
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("每天好懵逼地活着");
MODULE_DESCRIPTION("电力设备模拟驱动 - 阶段2");
MODULE_VERSION("1.0");

/* TODO 5 答案: 声明模块参数 */
static char *sensor_name = "power_sensor";
static int default_voltage = 220;

module_param(sensor_name, charp, 0644);
MODULE_PARM_DESC(sensor_name, "Sensor/Device name (default: power_sensor)");

module_param(default_voltage, int, 0644);
MODULE_PARM_DESC(default_voltage, "Default voltage in volts (default: 220)");

/* TODO 1 答案: 实现模块加载函数 */
static int __init power_init(void)
{
    printk(KERN_INFO "Power Device Module Loaded\n");

    /* TODO 6 答案: 在 power_init 中打印模块参数 */
    printk(KERN_INFO "Sensor name: %s, default voltage: %d V\n",
           sensor_name, default_voltage);

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
