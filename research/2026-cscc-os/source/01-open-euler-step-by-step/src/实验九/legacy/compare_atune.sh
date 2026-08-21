#!/bin/bash
# compare_atune.sh - A-Tune 调优前后对比测试
# 用途：自动执行调优前后对比，生成对比报告

set -e

echo "=========================================="
echo "  A-Tune 调优前后对比测试"
echo "=========================================="

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

RESULT_DIR="./results"
mkdir -p $RESULT_DIR
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

# ========================================
# TODO: 阶段1: A-Tune 关闭时测试
# ========================================
echo -e "\n${YELLOW}[阶段1] A-Tune 关闭 - 基准测试${NC}"

# TODO: 停止 A-Tune 服务
systemctl stop atuned 2>/dev/null || true
sleep 2

# TODO: 测试 CPU 计算
echo "  测试 CPU 计算 (圆周率)..."
CPU_OFF=$( { time echo "scale=5000; 4*a(1)" | bc -l > /dev/null 2>&1; } 2>&1 | grep real | awk '{print $2}')
echo "  CPU 耗时: ${CPU_OFF}"

# TODO: 测试内存带宽
echo "  测试内存带宽 (dd)..."
DD_OFF=$(dd if=/dev/zero of=/tmp/test bs=1M count=1024 conv=fdatasync 2>&1 | grep -oP '\d+\.\d+ GB/s' | head -1)
echo "  内存速度: ${DD_OFF}"
rm -f /tmp/test

# ========================================
# TODO: 阶段2: A-Tune 开启时测试
# ========================================
echo -e "\n${YELLOW}[阶段2] A-Tune 开启 - 调优测试${NC}"

# TODO: 启动 A-Tune 服务并应用模板
systemctl start atuned
sleep 2
atune-adm profile basic-test-suite-baseline-stream

# TODO: 测试 CPU 计算
echo "  测试 CPU 计算 (圆周率)..."
CPU_ON=$( { time echo "scale=5000; 4*a(1)" | bc -l > /dev/null 2>&1; } 2>&1 | grep real | awk '{print $2}')
echo "  CPU 耗时: ${CPU_ON}"

# TODO: 测试内存带宽
echo "  测试内存带宽 (dd)..."
DD_ON=$(dd if=/dev/zero of=/tmp/test bs=1M count=1024 conv=fdatasync 2>&1 | grep -oP '\d+\.\d+ GB/s' | head -1)
echo "  内存速度: ${DD_ON}"
rm -f /tmp/test

# ========================================
# TODO: 阶段3: 生成对比报告
# ========================================
REPORT_FILE="${RESULT_DIR}/compare_${TIMESTAMP}.txt"

cat > $REPORT_FILE << EOF
========================================
  A-Tune 调优对比报告
  生成时间: $(date)
========================================

| 测试项 | A-Tune 关闭 | A-Tune 开启 (stream) | 变化 |
|--------|------------|---------------------|------|
| CPU 计算 (圆周率) | ${CPU_OFF} | ${CPU_ON} | -- |
| 内存带宽 (dd) | ${DD_OFF} | ${DD_ON} | -- |

========================================
EOF

echo -e "\n=========================================="
echo -e "${GREEN}  对比测试完成！${NC}"
echo "报告已保存至: $REPORT_FILE"
echo "=========================================="

cat $REPORT_FILE