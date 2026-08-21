#!/bin/bash
# test_container_answer.sh - iSulad 容器引擎安装与测试（完整答案）
# 功能：一键安装 iSulad 并验证容器运行

set -e

echo "=========================================="
echo "  iSulad 容器引擎安装与测试脚本"
echo "=========================================="

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# 1. 检查系统是否为 openEuler
echo -e "\n[1/6] 检查系统环境..."
if [ ! -f /etc/openEuler-release ]; then
    echo -e "${RED}错误：当前系统不是 openEuler${NC}"
    exit 1
fi
cat /etc/openEuler-release
echo -e "${GREEN}✓ 系统检查通过${NC}"

# 2. 安装 iSulad 和 isula-build
echo -e "\n[2/6] 安装 iSulad 和 isula-build..."
dnf install -y iSulad isula-build
echo -e "${GREEN}✓ iSulad 安装完成${NC}"

# 3. 配置镜像加速
echo -e "\n[3/6] 配置镜像加速..."
mkdir -p /etc/isulad
cat > /etc/isulad/daemon.json << 'EOF'
{
    "registry-mirrors": [
        "https://docker.m.daocloud.io",
        "https://dockerproxy.com"
    ]
}
EOF
echo -e "${GREEN}✓ 镜像加速配置完成${NC}"

# 4. 启动 iSulad 服务
echo -e "\n[4/6] 启动 iSulad 服务..."
systemctl enable --now isulad
sleep 2
if systemctl is-active --quiet isulad; then
    echo -e "${GREEN}✓ iSulad 服务运行正常${NC}"
else
    echo -e "${RED}✗ iSulad 服务启动失败${NC}"
    systemctl status isulad
    exit 1
fi

# 5. 拉取测试镜像
echo -e "\n[5/6] 拉取测试镜像..."
isula pull hub.oepkgs.net/openeuler/openeuler:24.03
echo -e "${GREEN}✓ 镜像拉取完成${NC}"

# 6. 运行测试容器
echo -e "\n[6/6] 运行测试容器..."
isula run --rm hub.oepkgs.net/openeuler/openeuler:24.03 cat /etc/os-release
echo -e "${GREEN}✓ 容器运行测试通过${NC}"

echo -e "\n=========================================="
echo -e "${GREEN}  iSulad 安装配置完成！${NC}"
echo "=========================================="