#!/bin/bash
# A-Tune 智能调优实验 - 主测试脚本
# 用法: sudo ./test_atune_exp.sh

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

# 检查root权限
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}请使用 root 权限运行此脚本 (sudo ./test_atune_exp.sh)${NC}"
    exit 1
fi

echo "========== A-Tune 智能调优实验测试开始 =========="

# 测试1: 检查 A-Tune 是否已安装
((TOTAL++))
echo -n "检查 A-Tune 安装状态... "
if command -v atune-adm &> /dev/null; then
    test_pass "A-Tune 已安装"
else
    test_fail "A-Tune 未安装，请先运行 ./test_atune.sh"
fi

# 测试2: 检查 A-Tune 服务状态
((TOTAL++))
echo -n "检查 A-Tune 服务状态... "
if systemctl is-active --quiet atuned; then
    test_pass "A-Tune 服务运行正常"
else
    test_fail "A-Tune 服务未运行"
fi

# 测试3: 检查可用调优模板
((TOTAL++))
echo -n "检查调优模板列表... "
if atune-adm list &> /dev/null; then
    test_pass "调优模板列表可用"
else
    test_fail "调优模板列表不可用"
fi

# 测试4: 应用调优模板
((TOTAL++))
echo -n "应用 stream 调优模板... "
if atune-adm profile basic-test-suite-baseline-stream &> /dev/null; then
    test_pass "stream 模板应用成功"
else
    test_fail "stream 模板应用失败"
fi

# 测试5: 基准测试脚本是否存在
((TOTAL++))
echo -n "检查 run_benchmark.sh 脚本... "
if [ -f run_benchmark.sh ] && [ -x run_benchmark.sh ]; then
    test_pass "run_benchmark.sh 存在且可执行"
else
    test_fail "run_benchmark.sh 缺失或不可执行"
fi

# 测试6: 对比测试脚本是否存在
((TOTAL++))
echo -n "检查 compare_atune.sh 脚本... "
if [ -f compare_atune.sh ] && [ -x compare_atune.sh ]; then
    test_pass "compare_atune.sh 存在且可执行"
else
    test_fail "compare_atune.sh 缺失或不可执行"
fi

# 测试7: 容器测试脚本是否存在
((TOTAL++))
echo -n "检查 test_container.sh 脚本... "
if [ -f test_container.sh ] && [ -x test_container.sh ]; then
    test_pass "test_container.sh 存在且可执行"
else
    test_fail "test_container.sh 缺失或不可执行"
fi

# 测试8: 执行快速基准测试（缩短版，只跑小规模）
((TOTAL++))
echo -n "执行快速基准测试... "
if timeout 30 ./run_benchmark.sh &> /dev/null; then
    test_pass "基准测试执行成功"
else
    test_fail "基准测试执行失败或超时"
fi

# 汇总
echo "========== 测试汇总 =========="
echo "总共: $TOTAL 项, ${GREEN}通过: $PASS${NC} 项, ${RED}失败: $FAIL${NC} 项"
if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}所有测试通过！${NC}"
else
    echo -e "${RED}有 $FAIL 项测试未通过，请检查。${NC}"
fi