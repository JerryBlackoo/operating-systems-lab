#!/bin/bash
# test_exp3.sh - 实验三测试脚本
# 用法: ./test_exp3.sh

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

echo "========== 实验三测试开始 =========="

# 测试1: 编译starter
((TOTAL++))
echo -n "编译 starter... "
cd starter && make clean > /dev/null 2>&1 && make > /dev/null 2>&1
if [ -f getpid ] && [ -f threadid ]; then
    test_pass "starter 编译成功"
else
    test_fail "starter 编译失败"
fi
cd ..

# 测试2: 运行getpid
((TOTAL++))
echo -n "测试 getpid... "
if timeout 2 ./starter/getpid > /dev/null 2>&1; then
    test_pass "getpid 运行正常"
else
    test_fail "getpid 运行异常"
fi

# 测试3: 运行threadid
((TOTAL++))
echo -n "测试 threadid... "
if timeout 2 ./starter/threadid > /dev/null 2>&1; then
    test_pass "threadid 运行正常"
else
    test_fail "threadid 运行异常"
fi

# 测试4: 编译solution
((TOTAL++))
echo -n "编译 solution... "
cd solution && make clean > /dev/null 2>&1 && make > /dev/null 2>&1
if [ -f getpid ] && [ -f threadid ]; then
    test_pass "solution 编译成功"
else
    test_fail "solution 编译失败"
fi
cd ..

# 测试5: zombie
((TOTAL++))
echo -n "测试 zombie... "
if [ -f starter/zombie ] && timeout 3 ./starter/zombie > /dev/null 2>&1; then
    test_pass "zombie 运行正常"
else
    test_fail "zombie 运行异常"
fi

# 测试6: spinlock
((TOTAL++))
echo -n "测试 spinlock... "
if [ -f starter/spinlock ] && timeout 5 ./starter/spinlock > /dev/null 2>&1; then
    test_pass "spinlock 运行正常"
else
    test_fail "spinlock 运行异常"
fi

# 测试7: producer_consumer
((TOTAL++))
echo -n "测试 producer_consumer... "
if [ -f starter/producer_consumer ] && timeout 5 ./starter/producer_consumer > /dev/null 2>&1; then
    test_pass "producer_consumer 运行正常"
else
    test_fail "producer_consumer 运行异常"
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