#!/bin/bash
# run_benchmark_answer.sh - 性能基准测试脚本（完整答案）
# 功能：测试 CPU 计算和内存带宽性能

set -e

echo "=========================================="
echo "  性能基准测试脚本"
echo "=========================================="

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

RESULT_DIR="./results"
mkdir -p $RESULT_DIR
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="${RESULT_DIR}/benchmark_${TIMESTAMP}.csv"

echo -e "\n结果将保存至: ${RESULT_FILE}"

echo "测试类型,状态,耗时(秒),速度(GB/s)" > $RESULT_FILE

# 测试1: CPU 计算性能（圆周率）
echo -e "\n${YELLOW}[测试1] CPU 计算性能 - 圆周率 5000 位${NC}"

CPU_TIME=$( { time echo "scale=5000; 4*a(1)" | bc -l > /dev/null 2>&1; } 2>&1 | grep real | awk '{print $2}' | sed 's/m/*60+/g' | sed 's/s//g' | bc -l 2>/dev/null || echo "N/A")
echo "  CPU 计算耗时: ${CPU_TIME}s"
echo "CPU,${CPU_TIME},," >> $RESULT_FILE

# 测试2: 内存带宽（dd 写入）
echo -e "\n${YELLOW}[测试2] 内存带宽 - dd 写入 1GB${NC}"

DD_OUTPUT=$(dd if=/dev/zero of=/tmp/test bs=1M count=1024 conv=fdatasync 2>&1)
DD_SPEED=$(echo "$DD_OUTPUT" | grep -oP '\d+\.\d+ GB/s' | head -1 || echo "N/A")
echo "  内存写入速度: ${DD_SPEED}"
echo "Memory,N/A,${DD_SPEED}" >> $RESULT_FILE
rm -f /tmp/test

# 测试3: 容器启动性能
echo -e "\n${YELLOW}[测试3] 容器启动性能${NC}"

START=$(date +%s.%N)
isula run --rm hub.oepkgs.net/openeuler/openeuler:24.03 echo "Hello" > /dev/null 2>&1
END=$(date +%s.%N)
CONTAINER_TIME=$(echo "$END - $START" | bc -l)
echo "  容器启动耗时: ${CONTAINER_TIME}s"
echo "Container,${CONTAINER_TIME},," >> $RESULT_FILE

echo -e "\n=========================================="
echo -e "${GREEN}  基准测试完成！${NC}"
echo "结果已保存至: $RESULT_FILE"
echo "=========================================="

cat $RESULT_FILE