#!/usr/bin/env bash
# test_power_sensor.sh - 电力传感器驱动模块自动化测试

MODULE="power_sensor"
DEVICE="/dev/power_sensor"

# 颜色定义
RED='\033[31m'
GREEN='\033[32m'
BLUE='\033[34m'
NC='\033[0m'

# 统计变量
TOTAL=0
PASS=0
FAIL=0

test_pass() {
    echo -e "[${GREEN}PASS${NC}] $1"
    ((PASS++))
}

test_fail() {
    echo -e "[${RED}FAIL${NC}] $1"
    ((FAIL++))
}

test_start() {
    ((TOTAL++))
}

cleanup() {
    echo ""
    echo "正在清理测试环境..."
    sudo rmmod ${MODULE} 2>/dev/null || true
    echo "清理完成！"
}

trap cleanup EXIT

echo "=========================================="
echo "    电力传感器驱动模块自动化测试"
echo "    (智能电网场景)"
echo "=========================================="
echo ""

# ==========================================
# 测试1: 检查编译环境
# ==========================================
echo "=== 测试1: 检查编译环境 ==="
test_start

if command -v gcc &> /dev/null; then
    test_pass "gcc 已安装"
else
    test_fail "gcc 未安装"
fi

if command -v make &> /dev/null; then
    test_pass "make 已安装"
else
    test_fail "make 未安装"
fi

if [ -d "/lib/modules/$(uname -r)/build" ]; then
    test_pass "内核头文件已安装"
else
    test_fail "内核头文件未安装"
fi
echo ""

# ==========================================
# 测试2: 编译电力传感器模块
# ==========================================
echo "=== 测试2: 编译电力传感器模块 ==="
test_start

if [ ! -f "power_sensor.c" ]; then
    test_fail "power_sensor.c 不存在"
    exit 1
fi

make clean > /dev/null 2>&1
rm -f power_sensor.ko power_sensor.o power_sensor.mod.c power_sensor.mod.o
make > /tmp/power_make.log 2>&1

if [ -f "power_sensor.ko" ]; then
    test_pass "编译成功 (power_sensor.ko)"
else
    test_fail "编译失败"
    echo "   错误信息："
    tail -10 /tmp/power_make.log | sed 's/^/   /'
fi
echo ""

# ==========================================
# 测试3: 加载电力传感器模块
# ==========================================
echo "=== 测试3: 加载电力传感器模块 ==="
test_start

if [ ! -f "power_sensor.ko" ]; then
    test_fail "模块文件不存在，跳过加载"
else
    sudo rmmod ${MODULE} 2>/dev/null || true
    sudo insmod power_sensor.ko 2>/dev/null

    if lsmod | grep -q "^${MODULE} "; then
        test_pass "模块加载成功"
        echo "   内核日志："
        sudo dmesg | grep "power_sensor" | tail -3 | sed 's/^/   /'
    else
        test_fail "模块加载失败"
        echo "   错误信息："
        sudo dmesg | tail -5 | sed 's/^/   /'
    fi
fi
echo ""

# ==========================================
# 测试4: 检查设备文件
# ==========================================
echo "=== 测试4: 检查设备文件 ==="
test_start

if [ -e "$DEVICE" ]; then
    test_pass "设备文件存在 (${DEVICE})"
    ls -l ${DEVICE} | awk '{print "   设备信息: "$1" "$5" "$6" "$7" "$8" "$9" "$10}'
else
    test_fail "设备文件不存在"
fi
echo ""

# ==========================================
# 测试5: 默认传感器读取（电压）
# ==========================================
echo "=== 测试5: 默认传感器读取（电压） ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    SENSOR_DATA=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
    
    if echo "$SENSOR_DATA" | grep -q "电压传感器"; then
        test_pass "读取电压数据成功"
        echo "   数据: $SENSOR_DATA"
    else
        test_fail "读取电压数据失败"
        echo "   实际: $SENSOR_DATA"
    fi
fi
echo ""

# ==========================================
# 测试6: 切换传感器 -> 电流
# ==========================================
echo "=== 测试6: 切换传感器类型 -> 电流 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    echo "current" | sudo tee ${DEVICE} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        SENSOR_DATA=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
        if echo "$SENSOR_DATA" | grep -q "电流传感器"; then
            test_pass "切换到电流传感器成功"
            echo "   数据: $SENSOR_DATA"
        else
            test_fail "切换到电流传感器失败"
            echo "   实际: $SENSOR_DATA"
        fi
    else
        test_fail "切换命令发送失败"
    fi
fi
echo ""

# ==========================================
# 测试7: 切换传感器 -> 温度
# ==========================================
echo "=== 测试7: 切换传感器类型 -> 温度 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    echo "temp" | sudo tee ${DEVICE} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        SENSOR_DATA=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
        if echo "$SENSOR_DATA" | grep -q "温度传感器"; then
            test_pass "切换到温度传感器成功"
            echo "   数据: $SENSOR_DATA"
        else
            test_fail "切换到温度传感器失败"
            echo "   实际: $SENSOR_DATA"
        fi
    else
        test_fail "切换命令发送失败"
    fi
fi
echo ""

# ==========================================
# 测试8: 切换传感器 -> 电压（恢复）
# ==========================================
echo "=== 测试8: 切换传感器类型 -> 电压 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    echo "voltage" | sudo tee ${DEVICE} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        SENSOR_DATA=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
        if echo "$SENSOR_DATA" | grep -q "电压传感器"; then
            test_pass "切换到电压传感器成功"
            echo "   数据: $SENSOR_DATA"
        else
            test_fail "切换到电压传感器失败"
            echo "   实际: $SENSOR_DATA"
        fi
    else
        test_fail "切换命令发送失败"
    fi
fi
echo ""

# ==========================================
# 测试9: 连续数据采集
# ==========================================
echo "=== 测试9: 连续数据采集（模拟实时监控） ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    DATA1=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
    DATA2=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
    DATA3=$(sudo cat ${DEVICE} 2>/dev/null | head -1)
    
    echo "   第1次: $DATA1"
    echo "   第2次: $DATA2"
    echo "   第3次: $DATA3"
    
    if echo "$DATA1" | grep -q "电压" && echo "$DATA2" | grep -q "电压" && echo "$DATA3" | grep -q "电压"; then
        test_pass "连续数据采集成功"
    else
        test_fail "连续数据采集异常"
    fi
fi
echo ""

# ==========================================
# 测试10: 无效命令处理
# ==========================================
echo "=== 测试10: 无效命令处理 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    echo "invalid_command" | sudo tee ${DEVICE} > /dev/null 2>&1
    if [ $? -ne 0 ]; then
        test_pass "无效命令被正确拒绝"
    else
        test_fail "无效命令未被拒绝"
    fi
fi
echo ""

# ==========================================
# 测试11: 验证内核日志
# ==========================================
echo "=== 测试11: 验证传感器切换日志 ==="
test_start

if sudo dmesg | grep -q "切换到电压传感器"; then
    test_pass "电压切换日志记录正常"
else
    test_fail "电压切换日志未记录"
fi

if sudo dmesg | grep -q "切换到电流传感器"; then
    test_pass "电流切换日志记录正常"
else
    test_fail "电流切换日志未记录"
fi

if sudo dmesg | grep -q "切换到温度传感器"; then
    test_pass "温度切换日志记录正常"
else
    test_fail "温度切换日志未记录"
fi
echo ""

# ==========================================
# 测试12: 卸载模块
# ==========================================
echo "=== 测试12: 卸载电力传感器模块 ==="
test_start

sudo rmmod ${MODULE} 2>/dev/null
if ! lsmod | grep -q "^${MODULE} "; then
    test_pass "模块卸载成功"
else
    test_fail "模块卸载失败"
fi
echo ""

# ==========================================
# 汇总报告
# ==========================================
echo "=========================================="
echo "    电力场景测试汇总报告"
echo "=========================================="
echo -e "${BLUE}汇总统计：(总共${TOTAL}项，通过${PASS}项，失败${FAIL}项)${NC}"
echo ""

if [ ${FAIL} -eq 0 ]; then
    echo -e "${GREEN}🎉 所有电力场景测试通过！${NC}"
else
    echo -e "${RED}存在 ${FAIL} 项测试失败，请检查驱动代码。${NC}"
fi

cleanup