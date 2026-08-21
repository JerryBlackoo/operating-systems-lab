#!/bin/bash
# 实验二 - 一键自动部署脚本
# 用法: cd src/实验二 && ./deploy.sh [starter|solution]
# 功能: 环境检查 → 编译 → 加载 → 验证 → 测试 → 清理
# 作者: 宋雅笛  更新日期: 2026-08-16

set -e

VERSION=${1:-solution}    # 默认 solution，可指定 starter
DEVICE="/dev/power_device"
TARGET_DIR="${VERSION}/cp3_device"
# 模块名取自 .ko 文件名（不带后缀），由 insmod 成功后动态确定
MODULE_NAME=""

# 在脚本最开始就保存脚本所在目录的绝对路径，避免后续 cd 后路径失效
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ========== 颜色定义 ==========
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# ========== 工具函数 ==========
log_info()  { echo -e "${BLUE}[INFO]${NC}  $1"; }
log_ok()     { echo -e "${GREEN}[OK]${NC}    $1"; }
log_warn()   { echo -e "${YELLOW}[WARN]${NC}  $1"; }
log_fail()   { echo -e "${RED}[FAIL]${NC}  $1"; }

# ========== 步骤1: 环境检查 ==========
echo ""
echo "=============================================="
echo "  实验二 · 内核模块编程 - 自动部署 (${VERSION})"
echo "  场景: 智能变电站电力设备驱动"
echo "=============================================="
echo ""

log_info "步骤1/6: 环境检查..."

# 检查内核版本
KERNEL_VER=$(uname -r)
log_info "  当前内核: ${KERNEL_VER}"

# 检查 kernel-devel
if [ ! -d "/lib/modules/${KERNEL_VER}/build" ]; then
    log_fail "  kernel-devel 未安装，请执行: sudo dnf install kernel-devel-$(uname -r)"
    exit 1
fi
log_ok "  kernel-devel 已安装"

# 检查 gcc
if ! command -v gcc &> /dev/null; then
    log_fail "  gcc 未安装"
    exit 1
fi
log_ok "  gcc $(gcc --version | head -1 | awk '{print $3}')"

# 检查 root 权限
if [ "$EUID" -ne 0 ]; then
    log_warn "  非root用户，insmod/rmmod 操作将使用 sudo"
fi

# ========== 步骤2: 清理旧模块 ==========
log_info "步骤2/6: 清理旧的模块状态..."
for m in power_device_cp3 power_device_cp2 power_device_cp1 power_device; do
    if lsmod | grep -q "^${m} "; then
        sudo rmmod ${m} 2>/dev/null && log_ok "  已卸载旧模块 ${m}"
    fi
done
log_ok "  无残留模块"

# ========== 步骤3: 编译 ==========
log_info "步骤3/6: 编译内核模块..."
cd "${SCRIPT_DIR}/${TARGET_DIR}"

if [ ! -f "Makefile" ]; then
    log_fail "  Makefile 不存在于 $(pwd)"
    exit 1
fi

make clean > /dev/null 2>&1
if make 2>&1 | grep -qi "error"; then
    log_fail "  编译失败"
    make 2>&1 | tail -20
    exit 1
fi

# 查找生成的 .ko 文件
KO_FILE=$(ls *.ko 2>/dev/null | head -1)
if [ -z "$KO_FILE" ]; then
    log_fail "  未生成 .ko 文件"
    exit 1
fi
log_ok "  编译成功: ${KO_FILE}"

# ========== 步骤4: 加载模块 ==========
log_info "步骤4/6: 加载内核模块..."
MODULE_NAME="${KO_FILE%.ko}"
sudo insmod ${KO_FILE}

if lsmod | grep -q "^${MODULE_NAME} "; then
    log_ok "  模块加载成功 (${MODULE_NAME})"
else
    log_fail "  模块加载失败"
    exit 1
fi

# 检查设备文件
if [ -e "${DEVICE}" ]; then
    log_ok "  设备文件已创建: ${DEVICE}"
else
    log_fail "  设备文件未创建: ${DEVICE}"
    sudo rmmod ${MODULE_NAME}
    exit 1
fi

# 检查 dmesg 日志
DMESG_LOG=$(sudo dmesg | tail -5 | grep -i "power_device")
if [ -n "$DMESG_LOG" ]; then
    log_ok "  dmesg 日志: ${DMESG_LOG}"
fi

# ========== 步骤5: 功能验证 ==========
log_info "步骤5/6: 电力场景功能验证..."

# 读取初始值（默认220V/10A）
OUTPUT=$(cat ${DEVICE} 2>/dev/null)
if echo "$OUTPUT" | grep -q "Voltage: 220 V" && echo "$OUTPUT" | grep -q "Current: 10 A"; then
    log_ok "  初始电力数据: 220V / 10A（默认值）"
else
    log_warn "  初始值异常: ${OUTPUT}"
fi

# 写入电力数据（模拟调整电压电流）
echo "380 15" | sudo tee ${DEVICE} > /dev/null
OUTPUT=$(cat ${DEVICE})
if echo "$OUTPUT" | grep -q "Power: 5700 W"; then
    log_ok "  功率计算正确: 380V × 15A = 5700W"
else
    log_fail "  功率计算错误: ${OUTPUT}"
fi

# 模拟过载场景
echo "10000 100" | sudo tee ${DEVICE} > /dev/null
OUTPUT=$(cat ${DEVICE})
if echo "$OUTPUT" | grep -q "Power: 1000000 W"; then
    log_ok "  过载场景测试: 10000V × 100A = 1000000W（已触发）"
fi

# ========== 步骤6: 完整测试 + 清理 ==========
log_info "步骤6/6: 运行完整测试脚本..."
# test_module.sh 内部会执行 make，必须在 cp3_device 目录运行
cd "${SCRIPT_DIR}/${TARGET_DIR}"
if [ -f "${SCRIPT_DIR}/tests/test_module.sh" ]; then
    bash "${SCRIPT_DIR}/tests/test_module.sh" || log_warn "  测试脚本存在失败项，请检查"
else
    log_warn "  测试脚本不存在"
fi

# 清理
log_info "清理模块..."
sudo rmmod ${MODULE_NAME} 2>/dev/null && log_ok "  模块已卸载" || log_warn "  模块卸载失败"

echo ""
log_ok "实验二自动部署完成！"
echo ""
echo "电力场景说明:"
echo "  - 设备 /dev/power_device 模拟智能变电站设备驱动"
echo "  - read  读取电压(V)、电流(A)、功率(W)"
echo "  - write 下发遥控指令 (格式: '电压 电流')"
echo "  - 功率自动计算: P = U × I"
echo ""
