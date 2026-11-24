include modules.mk

ROOT := $(abspath ..)
MODULES := $(foreach model,$(STANDALONE_MODELS),$(ROOT)/$(MODEL_DIR_$(model)))

.PHONY: all run clean models kernel run-kernel check

# 单模块镜像：每个 STANDALONE_MODELS 目录独立链接一个 QEMU ELF。
all run clean:
	@set -e; for module in $(MODULES); do $(MAKE) -C "$$module" $@; done

models: all

# 整内核镜像：由 micro/Makefile 按 BOOT_MODELS=all 链接全部模块。
kernel:
	$(MAKE) -C $(ROOT) BOOT_MODULES=all all

run-kernel: kernel
	$(MAKE) -C $(ROOT) BOOT_MODULES=all run

# 先逐个模块验收，再验收它们链接成的整内核。
check: run run-kernel
