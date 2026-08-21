/*
 * power_device_cp3_answer.c - 电力设备驱动模块（阶段3完整答案）
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ 阶段3 (CP3): 字符设备驱动 - 完整答案                       │
 * └─────────────────────────────────────────────────────────────┘
 *
 * 功能：完整的 misc 字符设备驱动，支持 read/write 操作
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/uaccess.h>
#include <linux/mutex.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("每天好懵逼地活着");
MODULE_DESCRIPTION("电力设备模拟驱动 - 阶段3");
MODULE_VERSION("1.0");

/* 模块参数（在阶段2基础上）
 * 注意：不能使用 dev_name / current 作为变量名 —— 它们在内核头里已经被定义为
 *       inline 函数和宏（x86 的 current 是 get_current()），会发生命名冲突。
 */
static char *sensor_name = "power_sensor";
static int default_voltage = 220;

module_param(sensor_name, charp, 0644);
MODULE_PARM_DESC(sensor_name, "Sensor/Device name");

module_param(default_voltage, int, 0644);
MODULE_PARM_DESC(default_voltage, "Default voltage in volts");

/* 电力数据结构
 * 电流字段用 amps 而不是 current，避免与 x86 <asm/current.h> 的 current 宏冲突。
 */
struct power_data {
    int voltage;    /* 电压 (V) */
    int amps;       /* 电流 (A)  —— 不要取名 current，和内核宏冲突 */
    int power;      /* 功率 (W) = voltage * amps */
};

static struct power_data dev_data = {
    .voltage = 220,
    .amps    = 10,
    .power   = 2200,
};

static DEFINE_MUTEX(data_mutex);

/* TODO 7 答案: 实现 read 函数 */
static ssize_t power_read(struct file *file, char __user *buf,
                          size_t len, loff_t *off)
{
    char kbuf[64];
    int ret;

    if (buf == NULL || len < 64)
        return -EINVAL;

    if (*off > 0)
        return 0;

    mutex_lock(&data_mutex);

    ret = snprintf(kbuf, sizeof(kbuf),
                   "Voltage: %d V\nCurrent: %d A\nPower: %d W\n",
                   dev_data.voltage, dev_data.amps, dev_data.power);

    if (ret < 0 || ret >= sizeof(kbuf)) {
        mutex_unlock(&data_mutex);
        return -EIO;
    }

    if (copy_to_user(buf, kbuf, ret)) {
        mutex_unlock(&data_mutex);
        return -EFAULT;
    }

    mutex_unlock(&data_mutex);

    *off += ret;
    return ret;
}

/* TODO 8 答案: 实现 write 函数 */
static ssize_t power_write(struct file *file, const char __user *buf,
                           size_t len, loff_t *off)
{
    char kbuf[32];
    int voltage, amps;

    if (buf == NULL || len > sizeof(kbuf))
        return -EINVAL;

    if (copy_from_user(kbuf, buf, len))
        return -EFAULT;

    kbuf[len] = '\0';

    if (sscanf(kbuf, "%d %d", &voltage, &amps) != 2) {
        printk(KERN_WARNING "Invalid input format: %s\n", kbuf);
        return -EINVAL;
    }

    mutex_lock(&data_mutex);

    dev_data.voltage = voltage;
    dev_data.amps    = amps;
    dev_data.power   = voltage * amps;

    printk(KERN_INFO "Power device updated: V=%d, I=%d, P=%d\n",
           voltage, amps, dev_data.power);

    mutex_unlock(&data_mutex);

    return len;
}

/* TODO 9 答案: 定义 file_operations 结构体 */
static const struct file_operations power_fops = {
    .owner = THIS_MODULE,
    .read  = power_read,
    .write = power_write,
};

/* TODO 10 答案: 定义 misc 设备结构体 */
static struct miscdevice power_misc = {
    .minor = MISC_DYNAMIC_MINOR,
    .name  = "power_device",
    .fops  = &power_fops,
    .mode  = 0666,
};

/* TODO 1 答案: 实现模块加载函数 */
static int __init power_init(void)
{
    int ret;

    printk(KERN_INFO "Power Device Module Loaded\n");
    printk(KERN_INFO "Sensor name: %s, default voltage: %d V\n",
           sensor_name, default_voltage);

    /* TODO 11 答案: 在 power_init 中注册 misc 设备 */
    ret = misc_register(&power_misc);
    if (ret) {
        printk(KERN_ERR "Failed to register misc device: %d\n", ret);
        return ret;
    }

    printk(KERN_INFO "Device file: /dev/%s created\n", power_misc.name);

    return 0;
}

/* TODO 2 答案: 实现模块卸载函数 */
static void __exit power_exit(void)
{
    /* TODO 12 答案: 在 power_exit 中注销 misc 设备 */
    misc_deregister(&power_misc);

    printk(KERN_INFO "Power Device Module Unloaded\n");
}

/* TODO 3 答案: 使用 module_init() 注册加载函数 */
module_init(power_init);

/* TODO 4 答案: 使用 module_exit() 注册卸载函数 */
module_exit(power_exit);
