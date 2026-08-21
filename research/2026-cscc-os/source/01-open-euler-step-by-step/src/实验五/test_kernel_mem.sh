#!/bin/bash
# 内核内存分配实验测试脚本 (kmalloc + vmalloc)
# 用法: ./test_kernel_mem.sh
# 注意: 需要在 root 权限下运行

GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

TOTAL=0
PASS=0
FAIL=0

test_pass() {
    echo -e "${GREEN}PASS${NC}: $1"
    ((PASS++))
}

test_fail() {
    echo -e "${RED}FAIL${NC}: $1"
    ((FAIL++))
}

# 检查root权限
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}请使用 root 权限运行此脚本 (sudo ./test_kernel_mem.sh)${NC}"
    exit 1
fi

echo "========== 内核内存分配实验测试开始 =========="

# 测试1: 编译内核模块
((TOTAL++))
echo -n "编译内核模块... "
make clean > /dev/null 2>&1
if make > /dev/null 2>&1; then
    test_pass "编译成功"
else
    test_fail "编译失败"
    exit 1
fi

# 测试2: kmalloc_test 模块加载
((TOTAL++))
echo -n "加载 kmalloc_test 模块... "
lsmod | grep -q kmalloc_test && rmmod kmalloc_test 2>/dev/null
if insmod kmalloc_test.ko 2>/dev/null; then
    test_pass "kmalloc_test 加载成功"
else
    test_fail "kmalloc_test 加载失败"
fi

# 测试3: 检查 kmalloc_test 内核日志
((TOTAL++))
echo -n "检查 kmalloc_test 内核日志... "
sleep 1
if dmesg | tail -20 | grep -q "kmalloc allocated memory at virtual address"; then
    test_pass "kmalloc_test 日志输出正确"
else
    test_fail "kmalloc_test 日志输出异常"
fi

# 测试4: kmalloc_test 模块卸载
((TOTAL++))
echo -n "卸载 kmalloc_test 模块... "
if rmmod kmalloc_test 2>/dev/null; then
    test_pass "kmalloc_test 卸载成功"
else
    test_fail "kmalloc_test 卸载失败"
fi

# 测试5: vmalloc_test 模块加载
((TOTAL++))
echo -n "加载 vmalloc_test 模块... "
lsmod | grep -q vmalloc_test && rmmod vmalloc_test 2>/dev/null
if insmod vmalloc_test.ko 2>/dev/null; then
    test_pass "vmalloc_test 加载成功"
else
    test_fail "vmalloc_test 加载失败"
fi

# 测试6: 检查 vmalloc_test 内核日志
((TOTAL++))
echo -n "检查 vmalloc_test 内核日志... "
sleep 1
if dmesg | tail -20 | grep -q "vmalloc allocated memory at virtual address"; then
    test_pass "vmalloc_test 日志输出正确"
else
    test_fail "vmalloc_test 日志输出异常"
fi

# 测试7: vmalloc_test 模块卸载
((TOTAL++))
echo -n "卸载 vmalloc_test 模块... "
if rmmod vmalloc_test 2>/dev/null; then
    test_pass "vmalloc_test 卸载成功"
else
    test_fail "vmalloc_test 卸载失败"
fi

# 清理
make clean > /dev/null 2>&1

# 汇总
echo "========== 测试汇总 =========="
echo "总共: $TOTAL 项, ${GREEN}通过: $PASS${NC} 项, ${RED}失败: $FAIL${NC} 项"
if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}所有测试通过！${NC}"
else
    echo -e "${RED}有 $FAIL 项测试未通过，请检查。${NC}"
fi