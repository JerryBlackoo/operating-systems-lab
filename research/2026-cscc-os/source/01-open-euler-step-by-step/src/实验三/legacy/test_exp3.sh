#!/bin/bash
# 实验三测试脚本
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

# 编译测试
echo "========== 实验三测试开始 =========="
((TOTAL++))
echo -n "编译所有程序... "
make clean > /dev/null 2>&1
if make > /dev/null 2>&1; then
    test_pass "编译成功"
else
    test_fail "编译失败"
fi

# 测试1: getpid
((TOTAL++))
echo -n "测试 getpid... "
if [ -f getpid ]; then
    test_pass "getpid 编译存在"
else
    test_fail "getpid 编译缺失"
fi

# 测试2: threadid
((TOTAL++))
echo -n "测试 threadid... "
if [ -f threadid ]; then
    test_pass "threadid 编译存在"
else
    test_fail "threadid 编译缺失"
fi

# 测试3: zombie
((TOTAL++))
echo -n "测试 zombie... "
if [ -f zombie ]; then
    test_pass "zombie 编译存在"
else
    test_fail "zombie 编译缺失"
fi

# 测试4: spinlock
((TOTAL++))
echo -n "测试 spinlock... "
if [ -f spinlock ]; then
    OUTPUT=$(timeout 5 ./spinlock 2>/dev/null)
    if echo "$OUTPUT" | grep -q "结果正确"; then
        test_pass "spinlock 运行正确"
    else
        test_fail "spinlock 运行异常"
    fi
else
    test_fail "spinlock 编译缺失"
fi

# 测试5: producer_consumer
((TOTAL++))
echo -n "测试 producer_consumer... "
if [ -f producer_consumer ]; then
    OUTPUT=$(timeout 5 ./producer_consumer 2>/dev/null)
    if echo "$OUTPUT" | grep -q "所有生产者消费者完成"; then
        test_pass "producer_consumer 运行正确"
    else
        test_fail "producer_consumer 运行异常"
    fi
else
    test_fail "producer_consumer 编译缺失"
fi

# 测试6: send/receive (消息队列)
((TOTAL++))
echo -n "测试 send/receive... "
if [ -f send ] && [ -f receive ]; then
    # 启动接收端（后台）
    ./receive > /tmp/recv_out 2>&1 &
    RECV_PID=$!
    sleep 1
    # 发送消息
    echo "test" | ./send > /tmp/send_out 2>&1
    echo "end" | ./send > /tmp/send_out 2>&1
    sleep 1
    if grep -q "收到: test" /tmp/recv_out 2>/dev/null; then
        test_pass "send/receive 通信正常"
    else
        test_fail "send/receive 通信异常"
    fi
    kill $RECV_PID 2>/dev/null
    rm -f /tmp/recv_out /tmp/send_out
else
    test_fail "send/receive 编译缺失"
fi

# 测试7: write/read (共享内存)
((TOTAL++))
echo -n "测试 write/read... "
if [ -f write ] && [ -f read ]; then
    # 先启动读端（后台）
    ./read > /tmp/read_out 2>&1 &
    READ_PID=$!
    sleep 1
    # 写端写入
    echo "test_msg" | ./write > /tmp/write_out 2>&1
    echo "end" | ./write > /tmp/write_out 2>&1
    sleep 1
    if grep -q "读到: test_msg" /tmp/read_out 2>/dev/null; then
        test_pass "write/read 通信正常"
    else
        test_fail "write/read 通信异常"
    fi
    kill $READ_PID 2>/dev/null
    rm -f /tmp/read_out /tmp/write_out
else
    test_fail "write/read 编译缺失"
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