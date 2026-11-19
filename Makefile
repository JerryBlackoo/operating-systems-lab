CLANG ?= clang
BUILD_DIR := build
KERNEL := $(BUILD_DIR)/kernel.elf

# 优先使用系统的 ld.lld，其次 rust-lld，最后是 rustup 工具链自带的 rust-lld
# （系统 rustc 的 sysroot 可能不包含它）。
LLD ?= $(shell command -v ld.lld 2>/dev/null || \
          command -v rust-lld 2>/dev/null || \
          ls $$HOME/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -n 1)

# clang 根据 argv[0] 选择 lld 驱动，因此用 ld.lld 这个名字暴露链接器。
TOOLCHAIN_DIR := $(BUILD_DIR)/toolchain
LD_LLD := $(TOOLCHAIN_DIR)/ld.lld

CFLAGS := --target=riscv64-unknown-elf \
          -march=rv64imac_zicsr -mabi=lp64 -mcmodel=medany \
          -ffreestanding -fno-builtin -fno-stack-protector \
          -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
          -nostdlib -O2 -g -Wall -Wextra -Ikernel

LDFLAGS := -nostdlib \
           -Wl,-T,kernel/arch/riscv/linker.ld \
           -Wl,--no-relax \
           -Wl,-Map,$(BUILD_DIR)/kernel.map

C_SRCS := kernel/boot/boot.c \
          kernel/arch/riscv/uart.c \
          kernel/arch/riscv/sbi.c \
          kernel/interrupt/trap.c \
          kernel/exception/exception.c \
          kernel/clock/clock.c \
          kernel/memory/pmm.c
ASM_SRCS := kernel/arch/riscv/boot.S kernel/arch/riscv/trap.S
OBJS := $(patsubst %.c,$(BUILD_DIR)/%.o,$(C_SRCS)) \
        $(patsubst %.S,$(BUILD_DIR)/%.o,$(ASM_SRCS))

QEMU ?= qemu-system-riscv64
QEMUFLAGS ?= -machine virt -m 128M -nographic -bios default

all: $(KERNEL)

$(LD_LLD):
	@test -n "$(LLD)" || { echo "error: need ld.lld or rust-lld (or a Rust toolchain)"; exit 1; }
	@mkdir -p $(dir $@)
	@ln -sf $(LLD) $@

$(KERNEL): $(OBJS) $(LD_LLD) kernel/arch/riscv/linker.ld
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) $(LDFLAGS) --ld-path=$(LD_LLD) -fuse-ld=lld $(OBJS) -o $@
	@echo "built $@"

$(BUILD_DIR)/%.o: %.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/%.o: %.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

run: $(KERNEL)
	$(QEMU) $(QEMUFLAGS) -kernel $(KERNEL)

clean:
	rm -rf $(BUILD_DIR)

.PHONY: all run clean
