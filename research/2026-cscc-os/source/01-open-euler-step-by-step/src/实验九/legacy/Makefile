# A-Tune 实验辅助 Makefile
SHELL := /bin/bash
SCRIPTS := run_benchmark.sh compare_atune.sh test_container.sh test_atune.sh test_atune_exp.sh

.PHONY: all clean test install

all: $(SCRIPTS)
	@echo "所有脚本已就绪"

# 赋予所有脚本执行权限
$(SCRIPTS):
	chmod +x $@

test: all
	sudo ./test_atune_exp.sh

install:
	sudo ./test_atune.sh

container:
	sudo ./test_container.sh

benchmark:
	./run_benchmark.sh

compare:
	sudo ./compare_atune.sh

clean:
	rm -rf results/
	@echo "已清理 results 目录"

help:
	@echo "可用命令:"
	@echo "  make test       - 运行主测试脚本"
	@echo "  make install    - 安装 A-Tune"
	@echo "  make container  - 安装并测试 iSulad 容器"
	@echo "  make benchmark  - 运行基准测试"
	@echo "  make compare    - 运行调优前后对比测试"
	@echo "  make clean      - 清理测试结果"