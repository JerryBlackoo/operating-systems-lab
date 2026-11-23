# 独立的架构自检镜像；不参与 micro/Makefile 的正式启动路径。
CLANG ?= clang
QEMU ?= qemu-system-riscv64
QEMUFLAGS ?= -machine virt -m 128M -nographic -bios default
BUILD_DIR ?= ../../../build/riscv-arch-selftest

LLD_REAL := $(shell command -v ld.lld 2>/dev/null || \
            command -v rust-lld 2>/dev/null || \
            ls $$HOME/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -n 1)
LD_LLD := $(BUILD_DIR)/toolchain/ld.lld
KERNEL := $(BUILD_DIR)/kernel.elf
CFLAGS := --target=riscv64-unknown-elf -march=rv64imac_zicsr_zifencei \
          -mabi=lp64 -mcmodel=medany -ffreestanding -fno-builtin \
          -fno-stack-protector -fno-pie -fno-asynchronous-unwind-tables \
          -fno-unwind-tables -nostdlib -O2 -g -Wall -Wextra -MMD -MP -I.
OBJS := $(addprefix $(BUILD_DIR)/,selftest.o trap.o sbi.o boot_asm.o trap_asm.o context_asm.o)

.DEFAULT_GOAL := all
.PHONY: all run clean
all: $(KERNEL)

$(LD_LLD):
	@mkdir -p $(@D)
	@test -n "$(LLD_REAL)" || { echo '找不到 ld.lld 或 rust-lld' >&2; exit 1; }
	ln -sf $(LLD_REAL) $@

$(KERNEL): $(OBJS) link.ld $(LD_LLD)
	$(LD_LLD) -T link.ld --no-relax -o $@ $(OBJS)
	@echo "built: $@"

$(BUILD_DIR)/%.o: %.c
	@mkdir -p $(@D)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/%_asm.o: %.S
	@mkdir -p $(@D)
	$(CLANG) $(CFLAGS) -c $< -o $@

run: $(KERNEL)
	$(QEMU) $(QEMUFLAGS) -kernel $(KERNEL)

clean:
	rm -rf $(BUILD_DIR)

-include $(OBJS:.o=.d)
