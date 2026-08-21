/*
 * power_device_cp3.c - 电力设备驱动模块（阶段3：字符设备驱动）
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ 阶段3 (CP3): 字符设备驱动                                │
 * │ 目标：创建 /dev/power_device 设备文件，支持读写          │
 * └─────────────────────────────────────────────────────────────┘
 *
 * TODO 任务：补全以下代码
 *
 * 命名提示（给同学的坑提示，starter 里已经帮你避坑了）：
 *   · 不要用 dev_name 做变量名 —— <linux/device.h> 里已有同名 inline 函数
 *   · 不要用 current  做变量名 —— x86 <asm/current.h> 里 current 是宏 get_current()
 *     会把你写的 int current; 展开成 int get_current(); 直接炸
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/uaccess.h>
#include <linux/mutex.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("电力设备模拟驱动 - 阶段3");
MODULE_VERSION("1.0");

/* 模块参数（已完成） */
static char *sensor_name = "power_sensor";
static int default_voltage = 220;

module_param(sensor_name, charp, 0644);
MODULE_PARM_DESC(sensor_name, "Sensor/Device name");

module_param(default_voltage, int, 0644);
MODULE_PARM_DESC(default_voltage, "Default voltage in volts");

/* 电力数据结构（已完成） */
struct power_data {
    int voltage;
    int amps;       /* 电流 (A) —— 注意不要取 current，会和 x86 current 宏冲突 */
    int power;
};

static struct power_data dev_data = {
    .voltage = 220,
    .amps    = 10,
    .power   = 2200,
};

static DEFINE_MUTEX(data_mutex);

/* TODO 7: 实现 read 函数
 * 要求：
 *   1) 加锁保护数据
 *   2) 格式化输出："Voltage: %d V\nCurrent: %d A\nPower: %d W\n"
 *      （提示："Current"在这里是字符串字面量，没有命名冲突问题；
 *         结构体里请取 .amps 字段来读电流值）
 *   3) 使用 copy_to_user 复制到用户空间
 */
static ssize_t power_read(struct file *file, char __user *buf,
                          size_t len, loff_t *off)
{
    // 在这里添加你的代码
    
    return 0;
}

/* TODO 8: 实现 write 函数
 * 要求：
 *   1) 使用 copy_from_user 获取用户输入
 *   2) 解析 "电压 电流" 格式（局部变量用 int voltage, amps;）
 *   3) 加锁更新数据并计算功率
 */
static ssize_t power_write(struct file *file, const char __user *buf,
                           size_t len, loff_t *off)
{
    // 在这里添加你的代码
    
    return len;
}

/* TODO 9: 定义 file_operations 结构体
 * 要求：注册 read 和 write 函数
 */
static const struct file_operations power_fops = {
    .owner = THIS_MODULE,
    // 添加你的代码
    
};

/* TODO 10: 定义 misc 设备结构体
 * 要求：
 *   - 动态分配次设备号
 *   - 设备名为 "power_device"
 *   - 注册 file_operations
 */
static struct miscdevice power_misc = {
    .minor = MISC_DYNAMIC_MINOR,
    // 添加你的代码
    
};

/* TODO 1: 实现模块加载函数 power_init */
static int __init power_init(void)
{
    int ret;
    
    printk(KERN_INFO "Power Device Module Loaded\n");
    printk(KERN_INFO "Sensor name: %s, default voltage: %d V\n",
           sensor_name, default_voltage);
    
    /* TODO 11: 注册 misc 设备 */
    
    return 0;
}

/* TODO 2: 实现模块卸载函数 power_exit */
static void __exit power_exit(void)
{
    /* TODO 12: 注销 misc 设备 */
    
    printk(KERN_INFO "Power Device Module Unloaded\n");
}

/* TODO 3: 使用 module_init() 注册加载函数 */

/* TODO 4: 使用 module_exit() 注册卸载函数 */
