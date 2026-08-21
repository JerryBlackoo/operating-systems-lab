# ================================================================
# Makefile.power - 电力场景调度模拟专用Makefile
# 
# 使用方法：
#   make -f Makefile.power          # 编译电力场景程序
#   make -f Makefile.power clean    # 清理编译产物
#   make -f Makefile.power test     # 运行测试
# ================================================================

# 编译器设置
CC = gcc
CFLAGS = -Wall -Wextra -O2 -g
TARGET = power_grid
SOURCES = power_grid.c

# 默认目标
all: $(TARGET)

# 编译电力场景程序
$(TARGET): $(SOURCES)
	@echo "🔨 编译电力场景调度模拟程序..."
	$(CC) $(CFLAGS) -o $(TARGET) $(SOURCES)
	@echo "✅ 编译完成: $(TARGET)"
	@ls -lh $(TARGET)

# 运行测试
test: $(TARGET)
	@echo ""
	@echo "🧪 运行电力场景自动化测试..."
	@chmod +x tests/test_power_grid.sh
	@./tests/test_power_grid.sh

# 运行混合任务测试
run-mixed: $(TARGET)
	@echo ""
	@echo "⚡ 运行混合任务测试..."
	@./$(TARGET)

# 运行优先级任务测试
run-priority: $(TARGET)
	@echo ""
	@echo "⚡ 运行优先级任务测试..."
	@./$(TARGET) --priority

# 显示帮助
help: $(TARGET)
	@./$(TARGET) --help

# 清理
clean:
	@echo "🧹 清理编译产物..."
	@rm -f $(TARGET)
	@echo "✅ 清理完成"

# 安装（复制到系统路径）
install: $(TARGET)
	@echo "📦 安装 power_grid 到 ~/bin..."
	@mkdir -p ~/bin
	@cp $(TARGET) ~/bin/
	@echo "✅ 安装完成，请确保 ~/bin 在 PATH 中"

# 声明伪目标
.PHONY: all clean test help install run-mixed run-priority