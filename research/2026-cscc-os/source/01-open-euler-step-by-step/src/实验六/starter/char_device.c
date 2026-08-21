/*
 * char_device.c - 字符设备驱动模块
 * 
 * 功能：实现一个简单的字符设备，支持读写操作
 * 设备名：char_device
 * 缓冲区大小：1024 字节
 * 
 * TODO 任务：补全以下代码
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

/* TODO 1：实现设备打开函数 char_open
 * 要求：使用 printk(KERN_INFO) 打印 "char_device: Device opened."
 * 返回：0
 */
static int char_open(struct inode *inodep, struct file *filep)
{
    // 在这里添加你的代码

    return 0;
}

/* TODO 2：实现设备释放函数 char_release
 * 要求：使用 printk(KERN_INFO) 打印 "char_device: Device closed."
 * 返回：0
 */
static int char_release(struct inode *inodep, struct file *filep)
{
    // 在这里添加你的代码

    return 0;
}

/* TODO 3：实现设备读取函数 char_read
 * 要求：
 *   1. 检查偏移量是否超出缓冲区大小，若超出则返回 0
 *   2. 计算需要拷贝的字节数（取 len 和剩余缓冲区空间的最小值）
 *   3. 使用 copy_to_user() 将数据从内核缓冲区拷贝到用户空间
 *   4. 若拷贝失败返回 -EFAULT
 *   5. 更新偏移量并打印 "char_device: Sent X bytes to user."
 *   6. 返回实际拷贝的字节数
 */
static ssize_t char_read(struct file *filep, char __user *buffer, 
                         size_t len, loff_t *offset)
{
    // 在这里添加你的代码

}

/* TODO 4：实现设备写入函数 char_write
 * 要求：
 *   1. 检查偏移量是否超出缓冲区大小，若超出则返回 -ENOSPC
 *   2. 计算需要拷贝的字节数（取 len 和剩余缓冲区空间的最小值）
 *   3. 使用 copy_from_user() 将数据从用户空间拷贝到内核缓冲区
 *   4. 若拷贝失败返回 -EFAULT
 *   5. 更新偏移量并打印 "char_device: Received X bytes from user."
 *   6. 返回实际拷贝的字节数
 */
static ssize_t char_write(struct file *filep, const char __user *buffer,
                          size_t len, loff_t *offset)
{
    // 在这里添加你的代码

}

/* TODO 5：定义 file_operations 结构体 fops
 * 要求：将 char_open、char_release、char_read、char_write
 *       分别赋值给 .open、.release、.read、.write 字段
 */
static struct file_operations fops = {
    // 在这里添加你的代码
};

/* TODO 6：实现模块加载函数 char_device_init
 * 要求：
 *   1. 使用 register_chrdev() 注册字符设备（动态分配主设备号）
 *   2. 失败时打印错误信息并返回错误码
 *   3. 成功时打印 "char_device: Registered with major number X."
 *   4. 使用 class_create() 创建设备类
 *   5. 失败时清理已注册的设备并返回错误码
 *   6. 使用 device_create() 创建设备节点
 *   7. 失败时清理设备类和已注册的设备并返回错误码
 *   8. 成功时打印 "char_device: Device initialized successfully."
 *   9. 返回 0
 */
static int __init char_device_init(void)
{
    // 在这里添加你的代码

}

/* TODO 7：实现模块卸载函数 char_device_exit
 * 要求：
 *   1. 使用 device_destroy() 销毁设备节点
 *   2. 使用 class_destroy() 销毁设备类
 *   3. 使用 unregister_chrdev() 注销字符设备
 *   4. 打印 "char_device: Device unregistered and cleaned up."
 */
static void __exit char_device_exit(void)
{
    // 在这里添加你的代码

}

/* TODO 8：使用 module_init() 注册加载函数 */
/* TODO 9：使用 module_exit() 注册卸载函数 */