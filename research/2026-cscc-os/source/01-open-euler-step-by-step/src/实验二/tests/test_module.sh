#!/bin/bash
# test_module.sh - 自动化测试电力设备模块
# 用法: cd src/实验二/solution/cp3_device && bash ../../tests/test_module.sh

set -e

DEVICE="/dev/power_device"

echo "=== 测试1: 编译模块 ==="
make clean > /dev/null 2>&1
if make 2>&1 | grep -qi "error"; then
    echo -e "\033[31mFAIL\033[0m 编译失败"
    make 2>&1 | tail -10
    exit 1
fi
KO_FILE=$(ls *.ko 2>/dev/null | head -1)
MODULE="${KO_FILE%.ko}"
if [ -n "$KO_FILE" ] && [ -f "$KO_FILE" ]; then
    echo -e "\033[32mPASS\033[0m 编译成功 (${KO_FILE})"
else
    echo -e "\033[31mFAIL\033[0m 未生成 .ko 文件"
    exit 1
fi

echo "=== 测试2: 加载模块 ==="
sudo rmmod ${MODULE} 2>/dev/null || true
sudo insmod ${KO_FILE}
if lsmod | grep -q "^${MODULE} "; then
    echo -e "\033[32mPASS\033[0m 模块加载成功 (${MODULE})"
else
    echo -e "\033[31mFAIL\033[0m 模块加载失败"
    exit 1
fi

echo "=== 测试3: 检查设备文件 ==="
if [ -e "$DEVICE" ]; then
    echo -e "\033[32mPASS\033[0m 设备文件存在"
else
    echo -e "\033[31mFAIL\033[0m 设备文件不存在"
    sudo rmmod ${MODULE}
    exit 1
fi

echo "=== 测试4: 读取初始值 ==="
OUTPUT=$(cat "$DEVICE")
if echo "$OUTPUT" | grep -q "Voltage: 220 V" && \
   echo "$OUTPUT" | grep -q "Current: 10 A"; then
    echo -e "\033[32mPASS\033[0m 初始值正确"
else
    echo -e "\033[31mFAIL\033[0m 初始值错误: ${OUTPUT}"
    sudo rmmod ${MODULE}
    exit 1
fi

echo "=== 测试5: 写入并验证 ==="
echo "380 15" | sudo tee "$DEVICE" > /dev/null
OUTPUT=$(cat "$DEVICE")
if echo "$OUTPUT" | grep -q "Voltage: 380 V" && \
   echo "$OUTPUT" | grep -q "Current: 15 A" && \
   echo "$OUTPUT" | grep -q "Power: 5700 W"; then
    echo -e "\033[32mPASS\033[0m 写入后数据正确"
else
    echo -e "\033[31mFAIL\033[0m 写入后数据错误: ${OUTPUT}"
    sudo rmmod ${MODULE}
    exit 1
fi

echo "=== 测试6: 卸载模块 ==="
sudo rmmod ${MODULE}
if ! lsmod | grep -q "^${MODULE} "; then
    echo -e "\033[32mPASS\033[0m 模块卸载成功"
else
    echo -e "\033[31mFAIL\033[0m 模块卸载失败"
    exit 1
fi

echo -e "\n\033[32m所有测试通过！\033[0m"