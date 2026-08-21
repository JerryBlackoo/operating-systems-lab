/*
 * solution/kmalloc_test_answer.c - kmalloc 内存分配测试（完整答案）
 * 功能：使用 kmalloc 分配 128KB 内存，打印虚拟地址和物理地址
 */

#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/io.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("每天好懵逼地活着");
MODULE_DESCRIPTION("kmalloc 内存分配测试");
MODULE_VERSION("1.0");

static void *kmalloc_ptr = NULL;
#define ALLOC_SIZE 128 * 1024

static int __init kmalloc_test_init(void)
{
    printk(KERN_INFO "Loading kmalloc_test module.\n");

    kmalloc_ptr = kmalloc(ALLOC_SIZE, GFP_KERNEL);
    if (!kmalloc_ptr) {
        printk(KERN_ERR "kmalloc failed to allocate memory.\n");
        return -ENOMEM;
    }

    unsigned long phys_addr = virt_to_phys(kmalloc_ptr);
    printk(KERN_INFO "kmalloc allocated memory at virtual address: %p\n", kmalloc_ptr);
    printk(KERN_INFO "Corresponding physical address: %lx\n", phys_addr);

    return 0;
}

static void __exit kmalloc_test_exit(void)
{
    printk(KERN_INFO "Unloading kmalloc_test module.\n");

    if (kmalloc_ptr) {
        kfree(kmalloc_ptr);
        printk(KERN_INFO "kmalloc memory freed.\n");
    }
}

module_init(kmalloc_test_init);
module_exit(kmalloc_test_exit);