/*
 * char_device.c - 字符设备驱动模块（完整答案）
 * 
 * 功能：实现一个简单的字符设备，支持读写操作
 * 设备名：char_device
 * 缓冲区大小：1024 字节
 */

#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/fs.h>
#include <linux/uaccess.h>
#include <linux/device.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("A simple character device driver");
MODULE_VERSION("1.0");

#define DEVICE_NAME "char_device"
#define BUFFER_SIZE 1024

static int major_number;
static char device_buffer[BUFFER_SIZE];
static struct class *char_class;
static struct device *char_dev;

/* TODO 1 答案：实现设备打开函数 char_open */
static int char_open(struct inode *inodep, struct file *filep)
{
    printk(KERN_INFO "char_device: Device opened.\n");
    return 0;
}

/* TODO 2 答案：实现设备释放函数 char_release */
static int char_release(struct inode *inodep, struct file *filep)
{
    printk(KERN_INFO "char_device: Device closed.\n");
    return 0;
}

/* TODO 3 答案：实现设备读取函数 char_read */
static ssize_t char_read(struct file *filep, char __user *buffer, 
                         size_t len, loff_t *offset)
{
    size_t bytes_to_copy;

    if (*offset >= BUFFER_SIZE) {
        return 0;
    }

    bytes_to_copy = min(len, (size_t)(BUFFER_SIZE - *offset));
    if (copy_to_user(buffer, device_buffer + *offset, bytes_to_copy) != 0) {
        return -EFAULT;
    }

    *offset += bytes_to_copy;
    printk(KERN_INFO "char_device: Sent %zu bytes to user.\n", bytes_to_copy);
    return bytes_to_copy;
}

/* TODO 4 答案：实现设备写入函数 char_write */
static ssize_t char_write(struct file *filep, const char __user *buffer,
                          size_t len, loff_t *offset)
{
    size_t bytes_to_copy;

    if (*offset >= BUFFER_SIZE) {
        return -ENOSPC;
    }

    bytes_to_copy = min(len, (size_t)(BUFFER_SIZE - *offset));
    if (copy_from_user(device_buffer + *offset, buffer, bytes_to_copy) != 0) {
        return -EFAULT;
    }

    *offset += bytes_to_copy;
    printk(KERN_INFO "char_device: Received %zu bytes from user.\n", bytes_to_copy);
    return bytes_to_copy;
}

/* TODO 5 答案：定义 file_operations 结构体 fops */
static struct file_operations fops = {
    .open = char_open,
    .release = char_release,
    .read = char_read,
    .write = char_write,
};

/* TODO 6 答案：实现模块加载函数 char_device_init */
static int __init char_device_init(void)
{
    printk(KERN_INFO "char_device: Initializing the char_device.\n");

    major_number = register_chrdev(0, DEVICE_NAME, &fops);
    if (major_number < 0) {
        printk(KERN_ERR "char_device: Failed to register a major number.\n");
        return major_number;
    }
    printk(KERN_INFO "char_device: Registered with major number %d.\n", major_number);

    char_class = class_create(DEVICE_NAME);
    if (IS_ERR(char_class)) {
        unregister_chrdev(major_number, DEVICE_NAME);
        printk(KERN_ERR "char_device: Failed to create device class.\n");
        return PTR_ERR(char_class);
    }

    char_dev = device_create(char_class, NULL, MKDEV(major_number, 0), 
                             NULL, DEVICE_NAME);
    if (IS_ERR(char_dev)) {
        class_destroy(char_class);
        unregister_chrdev(major_number, DEVICE_NAME);
        printk(KERN_ERR "char_device: Failed to create device.\n");
        return PTR_ERR(char_dev);
    }

    printk(KERN_INFO "char_device: Device initialized successfully.\n");
    return 0;
}

/* TODO 7 答案：实现模块卸载函数 char_device_exit */
static void __exit char_device_exit(void)
{
    device_destroy(char_class, MKDEV(major_number, 0));
    class_destroy(char_class);
    unregister_chrdev(major_number, DEVICE_NAME);
    printk(KERN_INFO "char_device: Device unregistered and cleaned up.\n");
}

/* TODO 8 答案：使用 module_init() 注册加载函数 */
module_init(char_device_init);

/* TODO 9 答案：使用 module_exit() 注册卸载函数 */
module_exit(char_device_exit);