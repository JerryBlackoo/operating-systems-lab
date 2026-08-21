/*
 * starter/vmalloc_test.c - vmalloc 内存分配测试（学生填空版）
 * TODO: 补全代码，使用 vmalloc 分配内存并打印虚实地址
 */

#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/vmalloc.h>
#include <linux/io.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("每天好懵逼地活着");
MODULE_DESCRIPTION("vmalloc 内存分配测试");
MODULE_VERSION("1.0");

static void *vmalloc_ptr = NULL;
#define VMALLOC_SIZE 1024 * 1024

static int __init vmalloc_test_init(void)
{
    /* TODO: 打印模块加载信息 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */

    /* TODO: 使用 vmalloc 分配 1MB 内存 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
    
    if (!vmalloc_ptr) {
        printk(KERN_ERR "vmalloc failed to allocate memory.\n");
        return -ENOMEM;
    }

    /* TODO: 获取并打印虚拟地址对应的物理地址 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */

    return 0;
}

static void __exit vmalloc_test_exit(void)
{
    /* TODO: 打印模块卸载信息 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */

    /* TODO: 释放 vmalloc 分配的内存 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
}

/* TODO: 使用 module_init() 注册加载函数 */

/* TODO: 使用 module_exit() 注册卸载函数 */