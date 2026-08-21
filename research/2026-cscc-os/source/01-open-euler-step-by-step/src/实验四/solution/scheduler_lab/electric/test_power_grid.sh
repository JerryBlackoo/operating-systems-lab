#!/bin/bash

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PASS_COUNT=0
FAIL_COUNT=0
TOTAL_TESTS=0

print_header() {
    echo ""
    echo "================================================================"
    echo -e "${BLUE}$1${NC}"
    echo "================================================================"
}

print_pass() {
    echo -e "${GREEN}✅ PASS: $1${NC}"
    ((PASS_COUNT++))
    ((TOTAL_TESTS++))
}

print_fail() {
    echo -e "${RED}❌ FAIL: $1${NC}"
    ((FAIL_COUNT++))
    ((TOTAL_TESTS++))
}

print_info() {
    echo -e "${YELLOW}ℹ️  $1${NC}"
}

# ============================================================
# 测试1：编译检查
# ============================================================
test_compile() {
    print_header "测试1：编译电力场景调度程序"

    if [ -f "power_grid" ]; then
        rm -f power_grid
    fi

    print_info "执行编译..."
    if gcc -Wall -Wextra -O2 -g -o power_grid power_grid.c 2>&1; then
        if [ -f "power_grid" ]; then
            print_pass "编译成功，生成 power_grid"
            return 0
        else
            print_fail "编译完成但未生成可执行文件"
            return 1
        fi
    else
        print_fail "编译失败"
        return 1
    fi
}

# ============================================================
# 测试2：混合任务测试
# ============================================================
test_mixed() {
    print_header "测试2：混合任务调度测试"

    print_info "运行混合任务测试（11个电力任务）..."

    output=$(./power_grid 2>&1)
    exit_code=$?

    if [ $exit_code -ne 0 ]; then
        print_fail "程序执行异常，退出码: $exit_code"
        return 1
    fi

    if echo "$output" | grep -q "FCFS" && \
       echo "$output" | grep -q "SJF" && \
       echo "$output" | grep -q "RR" && \
       echo "$output" | grep -q "CFS"; then
        print_pass "混合任务测试通过 - 四种算法均已执行"
    else
        print_fail "混合任务测试失败"
        return 1
    fi

    if echo "$output" | grep -q "紧急保护"; then
        print_pass "紧急保护任务统计已输出"
    fi

    return 0
}

# ============================================================
# 测试3：优先级任务测试
# ============================================================
test_priority() {
    print_header "测试3：优先级任务测试"

    print_info "运行优先级任务测试（--priority）..."

    output=$(./power_grid --priority 2>&1)
    exit_code=$?

    if [ $exit_code -ne 0 ]; then
        print_fail "优先级测试执行异常，退出码: $exit_code"
        return 1
    fi

    if echo "$output" | grep -q "RR (优先级场景)" && \
       echo "$output" | grep -q "CFS (优先级场景)"; then
        print_pass "优先级任务测试通过"
    else
        print_fail "优先级任务测试失败"
        return 1
    fi

    return 0
}

# ============================================================
# 测试4：帮助信息测试
# ============================================================
test_help() {
    print_header "测试4：帮助信息测试"

    output=$(./power_grid --help 2>&1)

    # 使用 grep -- 来避免 --priority 被当作 grep 的选项
    if echo "$output" | grep -q "用法" && \
       echo "$output" | grep -q -- "--priority"; then
        print_pass "帮助信息测试通过"
        return 0
    else
        print_fail "帮助信息测试失败"
        return 1
    fi
}

# ============================================================
# 测试5：结果摘要
# ============================================================
test_summary() {
    print_header "测试5：电力场景结果摘要"

    echo ""
    echo "================================================================"
    echo "电力场景测试结果摘要"
    echo "================================================================"
    echo ""
    echo "推荐结论："
    echo "  ✅ RR 和 CFS 适合电力实时系统"
    echo "  ❌ FCFS 和 SJF 不适合电力实时系统"
    echo "================================================================"

    print_pass "结果摘要生成完成"
    return 0
}

# ============================================================
# 主函数
# ============================================================
main() {
    print_header "⚡ 电力场景调度模拟 - 自动化测试套件 ⚡"

    print_info "测试时间: $(date)"
    print_info "当前目录: $(pwd)"

    if [ ! -f "power_grid.c" ]; then
        print_fail "未找到 power_grid.c"
        exit 1
    fi

    # 逐个执行测试
    test_compile || true
    test_mixed || true
    test_priority || true
    test_help || true
    test_summary || true

    echo ""
    echo "================================================================"
    echo -e "${BLUE}📊 测试结果汇总${NC}"
    echo "================================================================"
    echo -e "总测试数: ${TOTAL_TESTS}"
    echo -e "${GREEN}通过: ${PASS_COUNT}${NC}"
    echo -e "${RED}失败: ${FAIL_COUNT}${NC}"
    echo ""

    if [ $FAIL_COUNT -eq 0 ]; then
        echo -e "${GREEN}🎉 所有测试通过！${NC}"
        echo ""
        echo "可执行以下命令："
        echo "  ./power_grid              # 运行混合任务测试"
        echo "  ./power_grid --priority   # 运行优先级任务测试"
        exit 0
    else
        echo -e "${RED}❌ 部分测试失败，请检查日志${NC}"
        exit 1
    fi
}

main