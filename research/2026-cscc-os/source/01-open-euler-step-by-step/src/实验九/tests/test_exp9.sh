#!/bin/bash
# test_exp9.sh - 实验九测试脚本
# 用法: sudo ./test_exp9.sh

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
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
    echo -e "${RED}请使用 root 权限运行此脚本 (sudo ./tests/test_exp9.sh)${NC}"
    exit 1
fi

echo "========== 实验九：容器与智能调优测试开始 =========="

# 测试1: 检查测试脚本是否存在
((TOTAL++))
echo -n "检查 starter 脚本... "
if [ -f starter/test_container.sh ] && [ -f starter/test_atune.sh ]; then
    test_pass "starter 脚本存在"
else
    test_fail "starter 脚本缺失"
fi

# 测试2: 检查 solution 脚本是否存在
((TOTAL++))
echo -n "检查 solution 脚本... "
if [ -f solution/test_container_answer.sh ] && [ -f solution/test_atune_answer.sh ]; then
    test_pass "solution 脚本存在"
else
    test_fail "solution 脚本缺失"
fi

# 测试3: 检查 benchmark 脚本
((TOTAL++))
echo -n "检查 benchmark 脚本... "
if [ -f starter/run_benchmark.sh ] && [ -f starter/compare_atune.sh ]; then
    test_pass "benchmark 脚本存在"
else
    test_fail "benchmark 脚本缺失"
fi

# 测试4: 脚本语法检查（bash -n）
((TOTAL++))
echo -n "检查脚本语法... "
for script in starter/*.sh solution/*.sh; do
    if ! bash -n "$script" 2>/dev/null; then
        test_fail "语法错误: $script"
        exit 1
    fi
done
test_pass "所有脚本语法正确"

# 测试5: 给脚本加执行权限
((TOTAL++))
echo -n "设置执行权限... "
chmod +x starter/*.sh solution/*.sh 2>/dev/null
if [ -x starter/test_container.sh ]; then
    test_pass "执行权限设置成功"
else
    test_fail "执行权限设置失败"
fi

echo "========== 测试汇总 =========="
echo "总共: $TOTAL 项, ${GREEN}通过: $PASS${NC} 项, ${RED}失败: $FAIL${NC} 项"
if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}所有测试通过！${NC}"
else
    echo -e "${RED}有 $FAIL 项测试未通过，请检查。${NC}"
fi