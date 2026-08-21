/*
 * char_device.mod.c - 模块元信息文件
 * 
 * 功能：包含模块的版本信息、依赖关系和符号表
 * 注意：此文件通常由内核构建系统自动生成
 * 
 * TODO 任务：补全以下代码
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

/* TODO 1：调用 BUILD_SALT 和 BUILD_LTO_INFO 宏 */
// 在这里添加你的代码

/* TODO 2：使用 MODULE_INFO 设置 vermagic 和 name */
// 在这里添加你的代码

/* TODO 3：定义 __this_module 结构体
 * 要求：
 *   .name = KBUILD_MODNAME
 *   .init = init_module
 *   .exit = cleanup_module（如果 CONFIG_MODULE_UNLOAD 已定义）
 *   .arch = MODULE_ARCH_INIT
 */
__visible struct module __this_module
__section(".gnu.linkonce.this_module") = {
    // 在这里添加你的代码
};

#ifdef CONFIG_RETPOLINE
MODULE_INFO(retpoline, "Y");
#endif

/* TODO 4：定义 ____versions 数组
 * 要求：列出本模块依赖的内核符号及其版本
 * 提示：参考以下格式
 *   { 0xdc4faa72, "__register_chrdev" },
 *   { 0x4e53b059, "class_create" },
 *   // ... 更多符号
 */
static const struct modversion_info ____versions[]
__used __section("__versions") = {
    // 在这里添加你的代码
};

/* TODO 5：使用 MODULE_INFO 设置模块依赖信息 */
// 在这里添加你的代码

/* TODO 6：使用 MODULE_INFO 设置 srcversion */
// 在这里添加你的代码