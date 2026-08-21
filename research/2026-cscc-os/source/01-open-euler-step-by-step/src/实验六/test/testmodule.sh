#!/bin/bash
# testmodule.sh - 字符设备驱动模块自动化测试

MODULE="char_device"
DEVICE="/dev/char_device"

# 颜色定义
RED='\033[31m'
GREEN='\033[32m'
YELLOW='\033[33m'
BLUE='\033[34m'
NC='\033[0m'

# 统计变量
TOTAL=0
PASS=0
FAIL=0

# 测试函数
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

# 清理函数
cleanup() {
    echo ""
    echo "正在清理测试环境..."
    rmmod ${MODULE} 2>/dev/null || true
    echo "清理完成！"
}

trap cleanup EXIT

echo "=========================================="
echo "    字符设备驱动模块自动化测试"
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
# 测试2: 修复驱动代码
# ==========================================
echo "=== 测试2: 修复驱动代码 ==="
test_start

if ! grep -q '#include <linux/string.h>' char_device.c; then
    sed -i '/#include <linux\/device.h>/a #include <linux/string.h>' char_device.c
    test_pass "已添加 string.h 头文件"
else
    test_pass "string.h 头文件已存在"
fi

if ! grep -q 'memset(device_buffer, 0, BUFFER_SIZE)' char_device.c; then
    sed -i '/static int __init char_device_init(void)/,/^}/ {
        /printk(KERN_INFO "char_device: Initializing/ a\
    memset(device_buffer, 0, BUFFER_SIZE);
    }' char_device.c
    test_pass "已添加缓冲区初始化代码"
else
    test_pass "缓冲区初始化代码已存在"
fi
echo ""

# ==========================================
# 测试3: 编译模块
# ==========================================
echo "=== 测试3: 编译模块 ==="
test_start

make clean > /dev/null 2>&1
make > /dev/null 2>&1

if [ -f "${MODULE}.ko" ]; then
    test_pass "编译成功 (${MODULE}.ko)"
else
    test_fail "编译失败"
    echo "   错误信息："
    make 2>&1 | tail -5
fi
echo ""

# ==========================================
# 测试4: 加载模块
# ==========================================
echo "=== 测试4: 加载模块 ==="
test_start

if [ ! -f "${MODULE}.ko" ]; then
    test_fail "模块文件不存在，跳过加载"
else
    rmmod ${MODULE} 2>/dev/null || true
    insmod ${MODULE}.ko 2>/dev/null

    if lsmod | grep -q "^${MODULE} "; then
        test_pass "模块加载成功"
    else
        test_fail "模块加载失败"
        echo "   错误信息："
        dmesg | tail -5 | grep -i "char_device" || echo "   无错误信息"
    fi
fi
echo ""

# ==========================================
# 测试5: 检查设备文件
# ==========================================
echo "=== 测试5: 检查设备文件 ==="
test_start

if [ -e "$DEVICE" ]; then
    test_pass "设备文件存在 (${DEVICE})"
else
    test_fail "设备文件不存在"
fi
echo ""

# ==========================================
# 测试6: 设备文件权限
# ==========================================
echo "=== 测试6: 设备文件权限 ==="
test_start

if [ -r "$DEVICE" ] && [ -w "$DEVICE" ]; then
    test_pass "设备文件可读写"
else
    test_fail "设备文件权限不足"
fi
echo ""

# ==========================================
# 测试7: 写入测试数据
# ==========================================
echo "=== 测试7: 写入测试数据 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    TEST_STRING="Hello, Character Device!"
    echo "$TEST_STRING" > ${DEVICE} 2>/dev/null

    if [ $? -eq 0 ]; then
        test_pass "数据写入成功"
    else
        test_fail "数据写入失败"
    fi
fi
echo ""

# ==========================================
# 测试8: 读取验证数据
# ==========================================
echo "=== 测试8: 读取验证数据 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    TEST_STRING="Hello, Character Device!"
    READ_DATA=$(cat ${DEVICE} 2>/dev/null | head -1)

    if [ "$READ_DATA" == "$TEST_STRING" ]; then
        test_pass "数据读取正确"
        echo "   写入: \"$TEST_STRING\""
        echo "   读取: \"$READ_DATA\""
    else
        test_fail "数据读取错误"
        echo "   期望: \"$TEST_STRING\""
        echo "   实际: \"$READ_DATA\""
    fi
fi
echo ""

# ==========================================
# 测试9: 多次写入和读取
# ==========================================
echo "=== 测试9: 多次写入和读取 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    WRITE1="First"
    WRITE2="Second"
    WRITE3="Third"

    rmmod ${MODULE} 2>/dev/null || true
    insmod ${MODULE}.ko 2>/dev/null
    echo "$WRITE1" > ${DEVICE} 2>/dev/null
    READ1=$(cat ${DEVICE} 2>/dev/null | head -1)

    rmmod ${MODULE} 2>/dev/null || true
    insmod ${MODULE}.ko 2>/dev/null
    echo "$WRITE2" > ${DEVICE} 2>/dev/null
    READ2=$(cat ${DEVICE} 2>/dev/null | head -1)

    rmmod ${MODULE} 2>/dev/null || true
    insmod ${MODULE}.ko 2>/dev/null
    echo "$WRITE3" > ${DEVICE} 2>/dev/null
    READ3=$(cat ${DEVICE} 2>/dev/null | head -1)

    if [ "$READ1" == "$WRITE1" ] && [ "$READ2" == "$WRITE2" ] && [ "$READ3" == "$WRITE3" ]; then
        test_pass "多次读写正确"
        echo "   1: \"$WRITE1\" -> \"$READ1\""
        echo "   2: \"$WRITE2\" -> \"$READ2\""
        echo "   3: \"$WRITE3\" -> \"$READ3\""
    else
        test_fail "多次读写错误"
        echo "   期望1: \"$WRITE1\", 实际1: \"$READ1\""
        echo "   期望2: \"$WRITE2\", 实际2: \"$READ2\""
        echo "   期望3: \"$WRITE3\", 实际3: \"$READ3\""
    fi
fi
echo ""

# ==========================================
# 测试10: 缓冲区边界测试
# ==========================================
echo "=== 测试10: 缓冲区边界测试 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    # 重新加载模块重置缓冲区
    rmmod ${MODULE} 2>/dev/null || true
    insmod ${MODULE}.ko 2>/dev/null
    
    LONG_STRING=$(printf 'A%.0s' {1..2048})
    echo "$LONG_STRING" > ${DEVICE} 2>/dev/null
    READ_LONG=$(cat ${DEVICE} 2>/dev/null)
    READ_LEN=${#READ_LONG}

    if [ $READ_LEN -le 1024 ] && [ $READ_LEN -gt 0 ]; then
        test_pass "缓冲区溢出保护正常"
        echo "   写入2048字节，实际写入${READ_LEN}字节"
    else
        test_fail "缓冲区溢出保护异常"
        echo "   写入2048字节，实际写入${READ_LEN}字节"
    fi
fi
echo ""

# ==========================================
# 测试11: 空数据测试
# ==========================================
echo "=== 测试11: 空数据测试 ==="
test_start

if [ ! -e "$DEVICE" ]; then
    test_fail "设备文件不存在，跳过测试"
else
    # 重新加载模块重置缓冲区（清除之前测试残留的数据）
    rmmod ${MODULE} 2>/dev/null || true
    insmod ${MODULE}.ko 2>/dev/null
    
    echo "" > ${DEVICE} 2>/dev/null
    READ_EMPTY=$(cat ${DEVICE} 2>/dev/null)

    if [ -z "$READ_EMPTY" ]; then
        test_pass "空数据处理正确"
    else
        test_fail "空数据处理错误"
        echo "   期望空字符串，实际: \"$READ_EMPTY\""
    fi
fi
echo ""

# ==========================================
# 测试12: 卸载模块
# ==========================================
echo "=== 测试12: 卸载模块 ==="
test_start

rmmod ${MODULE} 2>/dev/null
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
echo "    测试汇总报告"
echo "=========================================="
echo -e "总共测试项: ${BLUE}${TOTAL}${NC}"
echo -e "${GREEN}通过: ${PASS}${NC}"
echo -e "${RED}失败: ${FAIL}${NC}"
echo ""
echo -e "${BLUE}汇总统计：(总共${TOTAL}项，通过${PASS}项，失败${FAIL}项)${NC}"
echo ""

if [ ${FAIL} -eq 0 ]; then
    echo -e "${GREEN}所有测试通过！${NC}"
else
    echo -e "${RED}存在 ${FAIL} 项测试失败，请检查驱动代码。${NC}"
fi

cleanup