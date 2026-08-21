/*
 * solution/vmalloc_test_answer.c - vmalloc 内存分配测试（完整答案）
 * 功能：使用 vmalloc 分配 1MB 内存，打印虚拟地址和物理地址
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
    printk(KERN_INFO "Loading vmalloc_test module.\n");

    vmalloc_ptr = vmalloc(VMALLOC_SIZE);
    if (!vmalloc_ptr) {
        printk(KERN_ERR "vmalloc failed to allocate memory.\n");
        return -ENOMEM;
    }

    unsigned long phys_addr = virt_to_phys(vmalloc_ptr);
    printk(KERN_INFO "vmalloc allocated memory at virtual address: %p\n", vmalloc_ptr);
    printk(KERN_INFO "Corresponding physical address: %lx\n", phys_addr);

    return 0;
}

static void __exit vmalloc_test_exit(void)
{
    printk(KERN_INFO "Unloading vmalloc_test module.\n");

    if (vmalloc_ptr) {
        vfree(vmalloc_ptr);
        printk(KERN_INFO "vmalloc memory freed.\n");
    }
}

module_init(vmalloc_test_init);
module_exit(vmalloc_test_exit);