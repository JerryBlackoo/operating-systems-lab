#!/bin/bash
# test_atune.sh - A-Tune 智能调优框架安装与测试脚本
# 用途：一键安装 A-Tune 并执行基准测试

set -e

echo "=========================================="
echo "  A-Tune 智能调优框架安装与测试脚本"
echo "=========================================="

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# TODO: 安装 A-Tune 组件
echo -e "\n[1/5] 安装 A-Tune 组件..."
dnf install -y atune atune-engine atune-collector
echo -e "${GREEN}✓ A-Tune 安装完成${NC}"

# TODO: 启动 A-Tune 服务
echo -e "\n[2/5] 启动 A-Tune 服务..."
systemctl enable --now atuned
sleep 2
if systemctl is-active --quiet atuned; then
    echo -e "${GREEN}✓ A-Tune 服务运行正常${NC}"
else
    echo -e "${RED}✗ A-Tune 服务启动失败${NC}"
    systemctl status atuned
    exit 1
fi

# TODO: 查看可用调优模板
echo -e "\n[3/5] 查看可用调优模板..."
atune-adm list

# TODO: 应用调优模板
echo -e "\n[4/5] 应用调优模板 (stream)..."
atune-adm profile basic-test-suite-baseline-stream
echo -e "${GREEN}✓ 模板应用完成${NC}"

# TODO: 验证调优状态
echo -e "\n[5/5] 验证调优状态..."
atune-adm list | grep true || echo "未找到激活的模板"

echo -e "\n=========================================="
echo -e "${GREEN}  A-Tune 安装配置完成！${NC}"
echo "=========================================="