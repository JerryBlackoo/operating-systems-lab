#!/bin/bash
# test_exp5.sh - 实验五测试脚本
# 用法: sudo ./test_exp5.sh

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

if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}请使用 root 权限运行此脚本 (sudo ./tests/test_exp5.sh)${NC}"
    exit 1
fi

echo "========== 实验五：内核内存分配测试开始 =========="

# 测试1: 编译 starter
((TOTAL++))
echo -n "编译 starter... "
cd starter && make clean > /dev/null 2>&1 && make > /dev/null 2>&1
if [ -f kmalloc_test.ko ] && [ -f vmalloc_test.ko ]; then
    test_pass "starter 编译成功"
else
    test_fail "starter 编译失败"
fi
cd ..

# 测试2: 加载 kmalloc_test
((TOTAL++))
echo -n "加载 kmalloc_test... "
lsmod | grep -q kmalloc_test && rmmod kmalloc_test 2>/dev/null
if insmod starter/kmalloc_test.ko 2>/dev/null; then
    test_pass "kmalloc_test 加载成功"
else
    test_fail "kmalloc_test 加载失败"
fi

# 测试3: 检查 kmalloc 日志
((TOTAL++))
echo -n "检查 kmalloc 日志... "
sleep 1
if dmesg | tail -20 | grep -q "kmalloc allocated memory at virtual address"; then
    test_pass "kmalloc 日志正确"
else
    test_fail "kmalloc 日志缺失"
fi

# 测试4: 卸载 kmalloc_test
((TOTAL++))
echo -n "卸载 kmalloc_test... "
if rmmod kmalloc_test 2>/dev/null; then
    test_pass "kmalloc_test 卸载成功"
else
    test_fail "kmalloc_test 卸载失败"
fi

# 测试5: 加载 vmalloc_test
((TOTAL++))
echo -n "加载 vmalloc_test... "
lsmod | grep -q vmalloc_test && rmmod vmalloc_test 2>/dev/null
if insmod starter/vmalloc_test.ko 2>/dev/null; then
    test_pass "vmalloc_test 加载成功"
else
    test_fail "vmalloc_test 加载失败"
fi

# 测试6: 检查 vmalloc 日志
((TOTAL++))
echo -n "检查 vmalloc 日志... "
sleep 1
if dmesg | tail -20 | grep -q "vmalloc allocated memory at virtual address"; then
    test_pass "vmalloc 日志正确"
else
    test_fail "vmalloc 日志缺失"
fi

# 测试7: 卸载 vmalloc_test
((TOTAL++))
echo -n "卸载 vmalloc_test... "
if rmmod vmalloc_test 2>/dev/null; then
    test_pass "vmalloc_test 卸载成功"
else
    test_fail "vmalloc_test 卸载失败"
fi

# 清理
make clean > /dev/null 2>&1

echo "========== 测试汇总 =========="
echo "总共: $TOTAL 项, ${GREEN}通过: $PASS${NC} 项, ${RED}失败: $FAIL${NC} 项"
if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}所有测试通过！${NC}"
else
    echo -e "${RED}有 $FAIL 项测试未通过，请检查。${NC}"
fi