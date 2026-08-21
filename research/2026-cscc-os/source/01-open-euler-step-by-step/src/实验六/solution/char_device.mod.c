/*
 * char_device.mod.c - 模块元信息文件（完整答案）
 * 
 * 功能：包含模块的版本信息、依赖关系和符号表
 * 注意：此文件通常由内核构建系统自动生成
 */

#include <linux/module.h>
#define INCLUDE_VERMAGIC
#include <linux/build-salt.h>
#include <linux/elfnote-lto.h>
#include <linux/export-internal.h>
#include <linux/vermagic.h>
#include <linux/compiler.h>

#ifdef CONFIG_UNWINDER_ORC
#include <asm/orc_header.h>
ORC_HEADER;
#endif

/* TODO 1 答案：调用 BUILD_SALT 和 BUILD_LTO_INFO 宏 */
BUILD_SALT;
BUILD_LTO_INFO;

/* TODO 2 答案：使用 MODULE_INFO 设置 vermagic 和 name */
MODULE_INFO(vermagic, VERMAGIC_STRING);
MODULE_INFO(name, KBUILD_MODNAME);

/* TODO 3 答案：定义 __this_module 结构体 */
__visible struct module __this_module
__section(".gnu.linkonce.this_module") = {
    .name = KBUILD_MODNAME,
    .init = init_module,
#ifdef CONFIG_MODULE_UNLOAD
    .exit = cleanup_module,
#endif
    .arch = MODULE_ARCH_INIT,
};

#ifdef CONFIG_RETPOLINE
MODULE_INFO(retpoline, "Y");
#endif

/* TODO 4 答案：定义 ____versions 数组 */
static const struct modversion_info ____versions[]
__used __section("__versions") = {
    { 0xdc4faa72, "__register_chrdev" },
    { 0x4e53b059, "class_create" },
    { 0x7e9cf5a9, "device_create" },
    { 0x6bc3fbc0, "__unregister_chrdev" },
    { 0x535a05, "class_destroy" },
    { 0x174bda2c, "device_destroy" },
    { 0xbdfb6dbb, "__fentry__" },
    { 0x122c3a7e, "_printk" },
    { 0x5b8239ca, "__x86_return_thunk" },
    { 0x88db9f48, "__check_object_size" },
    { 0x13c49cc2, "_copy_from_user" },
    { 0x6b10bee1, "_copy_to_user" },
    { 0xc7c48db2, "module_layout" },
};

/* TODO 5 答案：使用 MODULE_INFO 设置模块依赖信息 */
MODULE_INFO(depends, "");

/* TODO 6 答案：使用 MODULE_INFO 设置 srcversion */
MODULE_INFO(srcversion, "DD5973017D5154445BBFEEB");