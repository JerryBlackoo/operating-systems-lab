/**
 * power_sensor.c — 电力传感器设备驱动
 * 
 * 模拟电力监控系统中的传感器设备驱动
 * 支持：电压传感器、电流传感器、温度传感器
 */

#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/fs.h>
#include <linux/uaccess.h>
#include <linux/device.h>
#include <linux/random.h>
#include <linux/string.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("Power Grid Sensor Device Driver");
MODULE_VERSION("1.0");

#define DEVICE_NAME "power_sensor"
#define BUFFER_SIZE 256

// 传感器类型定义
#define SENSOR_VOLTAGE  1   // 电压传感器
#define SENSOR_CURRENT  2   // 电流传感器
#define SENSOR_TEMP     3   // 温度传感器

static int major_number;
static struct class *sensor_class;
static struct device *sensor_device;

// 传感器数据缓冲区
static char sensor_buffer[BUFFER_SIZE];
static int sensor_type = SENSOR_VOLTAGE;  // 默认电压传感器

// 模拟读取传感器数据（使用整数避免浮点运算）
static void read_sensor_data(char *buffer, size_t len)
{
    int raw_value;
    int value;

    get_random_bytes(&raw_value, sizeof(raw_value));
    raw_value = raw_value & 0x7FFFFFFF;

    switch(sensor_type) {
        case SENSOR_VOLTAGE:
            // 模拟电压值：220V ± 10%
            value = 220 + (raw_value % 20) - 10;
            snprintf(buffer, len, "电压传感器: %d V\n", value);
            break;
        case SENSOR_CURRENT:
            // 模拟电流值：50A ± 20%
            value = 50 + (raw_value % 20) - 10;
            snprintf(buffer, len, "电流传感器: %d A\n", value);
            break;
        case SENSOR_TEMP:
            // 模拟温度值：25°C ± 10°
            value = 25 + (raw_value % 20) - 10;
            snprintf(buffer, len, "温度传感器: %d C\n", value);
            break;
        default:
            snprintf(buffer, len, "未知传感器类型\n");
    }
}

// ==================== 标准设备操作函数 ====================
static int sensor_open(struct inode *inodep, struct file *filep)
{
    printk(KERN_INFO "power_sensor: 设备已打开 (模拟：电力传感器就绪)\n");
    return 0;
}

static int sensor_release(struct inode *inodep, struct file *filep)
{
    printk(KERN_INFO "power_sensor: 设备已关闭 (模拟：释放传感器资源)\n");
    return 0;
}

static ssize_t sensor_read(struct file *filep, char __user *buffer,
                           size_t len, loff_t *offset)
{
    size_t bytes_to_copy;

    if (*offset >= BUFFER_SIZE) {
        return 0;
    }

    // 读取传感器数据
    read_sensor_data(sensor_buffer, BUFFER_SIZE);
    bytes_to_copy = min(len, (size_t)BUFFER_SIZE);

    if (copy_to_user(buffer, sensor_buffer, bytes_to_copy) != 0) {
        return -EFAULT;
    }

    *offset += bytes_to_copy;
    printk(KERN_INFO "power_sensor: 读取传感器数据 %zu 字节\n", bytes_to_copy);
    return bytes_to_copy;
}

static ssize_t sensor_write(struct file *filep, const char __user *buffer,
                            size_t len, loff_t *offset)
{
    char cmd_buffer[32] = {0};
    size_t bytes_to_copy = min(len, (size_t)(sizeof(cmd_buffer) - 1));

    if (copy_from_user(cmd_buffer, buffer, bytes_to_copy) != 0) {
        return -EFAULT;
    }

    // 解析命令：切换传感器类型
    if (strncmp(cmd_buffer, "voltage", 7) == 0) {
        sensor_type = SENSOR_VOLTAGE;
        printk(KERN_INFO "power_sensor: 切换到电压传感器\n");
    } else if (strncmp(cmd_buffer, "current", 7) == 0) {
        sensor_type = SENSOR_CURRENT;
        printk(KERN_INFO "power_sensor: 切换到电流传感器\n");
    } else if (strncmp(cmd_buffer, "temp", 4) == 0) {
        sensor_type = SENSOR_TEMP;
        printk(KERN_INFO "power_sensor: 切换到温度传感器\n");
    } else {
        printk(KERN_WARNING "power_sensor: 未知命令: %s\n", cmd_buffer);
        return -EINVAL;
    }

    *offset += bytes_to_copy;
    return bytes_to_copy;
}

// ==================== 文件操作结构体 ====================
static struct file_operations fops = {
    .open = sensor_open,
    .release = sensor_release,
    .read = sensor_read,
    .write = sensor_write,
};

// ==================== 模块初始化和退出 ====================
static int __init sensor_init(void)
{
    printk(KERN_INFO "power_sensor: 初始化电力传感器驱动...\n");
    printk(KERN_INFO "power_sensor: 场景说明 — 智能电网传感器数据采集\n");

    major_number = register_chrdev(0, DEVICE_NAME, &fops);
    if (major_number < 0) {
        printk(KERN_ERR "power_sensor: 注册设备号失败\n");
        return major_number;
    }
    printk(KERN_INFO "power_sensor: 注册主设备号 %d\n", major_number);

    sensor_class = class_create(DEVICE_NAME);
    if (IS_ERR(sensor_class)) {
        unregister_chrdev(major_number, DEVICE_NAME);
        printk(KERN_ERR "power_sensor: 创建设备类失败\n");
        return PTR_ERR(sensor_class);
    }

    sensor_device = device_create(sensor_class, NULL,
                                   MKDEV(major_number, 0),
                                   NULL, DEVICE_NAME);
    if (IS_ERR(sensor_device)) {
        class_destroy(sensor_class);
        unregister_chrdev(major_number, DEVICE_NAME);
        printk(KERN_ERR "power_sensor: 创建设备文件失败\n");
        return PTR_ERR(sensor_device);
    }

    printk(KERN_INFO "power_sensor: 驱动初始化成功！\n");
    printk(KERN_INFO "power_sensor: 设备节点: /dev/%s\n", DEVICE_NAME);
    printk(KERN_INFO "power_sensor: 命令示例: echo voltage > /dev/%s\n", DEVICE_NAME);
    printk(KERN_INFO "power_sensor:          echo current > /dev/%s\n", DEVICE_NAME);
    printk(KERN_INFO "power_sensor:          echo temp > /dev/%s\n", DEVICE_NAME);

    return 0;
}

static void __exit sensor_exit(void)
{
    device_destroy(sensor_class, MKDEV(major_number, 0));
    class_destroy(sensor_class);
    unregister_chrdev(major_number, DEVICE_NAME);
    printk(KERN_INFO "power_sensor: 电力传感器驱动已卸载\n");
}

module_init(sensor_init);
module_exit(sensor_exit);