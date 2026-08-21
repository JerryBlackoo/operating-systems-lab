#!/bin/bash

# 调度平台测试脚本
# 用途：自动编译、运行调度模拟器、生成CSV文件、并调用绘图脚本

set -e  # 遇到错误立即退出

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 打印带颜色的信息
print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

# 打印分隔线
print_separator() {
    echo "=========================================="
}

# 获取脚本所在目录的父目录（即项目根目录）
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

print_separator
echo "      调度算法性能对比平台 - 自动化测试"
print_separator

# ==================== 1. 检查环境 ====================
print_info "检查编译环境..."

# 检查 gcc 是否安装
if ! command -v gcc &> /dev/null; then
    print_error "gcc 未安装，请先安装: sudo dnf install gcc -y"
    exit 1
fi
print_success "gcc 已安装"

# 检查 python3 是否安装
if ! command -v python3 &> /dev/null; then
    print_warning "python3 未安装，将跳过绘图测试"
    PYTHON_AVAILABLE=false
else
    print_success "python3 已安装"
    PYTHON_AVAILABLE=true
    
    # 检查 matplotlib 是否安装
    if ! python3 -c "import matplotlib" &> /dev/null; then
        print_warning "matplotlib 未安装，将跳过绘图测试"
        print_info "如需绘图，请运行: pip3 install matplotlib"
        PYTHON_AVAILABLE=false
    else
        print_success "matplotlib 已安装"
    fi
fi

# ==================== 2. 清理旧文件 ====================
print_info "清理旧文件..."
cd "$PROJECT_ROOT"
make clean &> /dev/null || true
rm -f *_results.csv scheduler_comparison.png &> /dev/null || true
print_success "清理完成"

# ==================== 3. 编译代码 ====================
print_info "编译调度模拟器..."
if make; then
    print_success "编译成功"
else
    print_error "编译失败，请检查代码"
    exit 1
fi

# ==================== 4. 检查可执行文件 ====================
if [ ! -f "./simulator" ]; then
    print_error "可执行文件 simulator 不存在"
    exit 1
fi
print_success "可执行文件已生成"

# ==================== 5. 运行调度模拟器 ====================
print_info "运行调度模拟器..."
print_separator
./simulator
print_separator

# ==================== 6. 检查CSV文件是否生成 ====================
print_info "检查结果文件..."

CSV_FILES=("FCFS_results.csv" "SJF_results.csv" "RR_results.csv" "CFS_results.csv")
ALL_CSV_EXIST=true

for csv in "${CSV_FILES[@]}"; do
    if [ -f "$PROJECT_ROOT/$csv" ]; then
        print_success "已生成: $csv"
    else
        print_error "未生成: $csv"
        ALL_CSV_EXIST=false
    fi
done

if [ "$ALL_CSV_EXIST" = false ]; then
    print_error "部分CSV文件未生成，请检查模拟器代码"
    exit 1
fi

# ==================== 7. 显示CSV内容 ====================
print_info "CSV文件内容预览："
print_separator
for csv in "${CSV_FILES[@]}"; do
    if [ -f "$PROJECT_ROOT/$csv" ]; then
        echo ""
        echo "--- $csv ---"
        cat "$PROJECT_ROOT/$csv"
    fi
done
print_separator

# ==================== 8. 生成对比图表 ====================
if [ "$PYTHON_AVAILABLE" = true ]; then
    print_info "生成对比图表..."
    
    # 检查 plot.py 是否存在
    if [ -f "$PROJECT_ROOT/../tools/plot.py" ]; then
        cd "$PROJECT_ROOT"
        python3 "$PROJECT_ROOT/../tools/plot.py"
        
        if [ -f "$PROJECT_ROOT/scheduler_comparison.png" ]; then
            print_success "图表已生成: scheduler_comparison.png"
        else
            print_warning "图表生成失败，请检查 plot.py"
        fi
    elif [ -f "$PROJECT_ROOT/tools/plot.py" ]; then
        cd "$PROJECT_ROOT"
        python3 "$PROJECT_ROOT/tools/plot.py"
        
        if [ -f "$PROJECT_ROOT/scheduler_comparison.png" ]; then
            print_success "图表已生成: scheduler_comparison.png"
        else
            print_warning "图表生成失败，请检查 plot.py"
        fi
    else
        print_warning "未找到 plot.py，跳过绘图"
        print_info "请将 plot.py 放在 tools/ 目录下"
    fi
else
    print_warning "跳过绘图（缺少Python环境）"
fi

# ==================== 9. 显示统计结果摘要 ====================
print_info "测试结果摘要："
print_separator

for csv in "${CSV_FILES[@]}"; do
    if [ -f "$PROJECT_ROOT/$csv" ]; then
        # 读取CSV内容（跳过标题行）
        ALG=$(tail -n 1 "$PROJECT_ROOT/$csv" | cut -d',' -f1)
        TURNAROUND=$(tail -n 1 "$PROJECT_ROOT/$csv" | cut -d',' -f2)
        RESPONSE=$(tail -n 1 "$PROJECT_ROOT/$csv" | cut -d',' -f3)
        printf "%-8s | 平均周转时间: %8.2f | 平均响应时间: %8.2f\n" "$ALG" "$TURNAROUND" "$RESPONSE"
    fi
done

print_separator

# ==================== 10. 完成 ====================
print_separator
print_success "所有测试通过！"
echo ""
echo "生成的文件："
echo "  - 可执行文件: ./simulator"
echo "  - CSV结果: FCFS_results.csv, SJF_results.csv, RR_results.csv, CFS_results.csv"
if [ -f "$PROJECT_ROOT/scheduler_comparison.png" ]; then
    echo "  - 对比图表: scheduler_comparison.png"
fi
print_separator

exit 0