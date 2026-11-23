CLANG ?= clang
QEMU ?= qemu-system-riscv64
QEMUFLAGS ?= -machine virt -m 128M -smp 1 -nographic -monitor none -bios default
QEMU_TIMEOUT ?= 15s

SHARED := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
include $(SHARED)/modules.mk
BUILD_SETTINGS := $(MAKEFILE_LIST)
ARCH_DIR := $(SHARED)/../kernel/arch/riscv
BUILD_DIR ?= $(SHARED)/../build/models/$(DEMO)
ELF := $(BUILD_DIR)/demo.elf
LLD_REAL ?= $(shell command -v ld.lld || command -v rust-lld || \
                     ls "$(HOME)"/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -n 1)
LD_LLD := $(BUILD_DIR)/ld.lld

MODEL_FLAGS := --target=riscv64-unknown-elf -march=rv64imac_zicsr_zifencei \
               -mabi=lp64 -mcmodel=medany -mno-relax -msmall-data-limit=0 \
               -std=c11 -ffreestanding -fno-builtin -fno-stack-protector \
               -fno-pie -fno-unwind-tables -fno-asynchronous-unwind-tables \
               -O2 -g -Wall -Wextra -Werror -MD -MP \
               -I$(SHARED) -isystem $(ARCH_DIR) -DDEMO_SYMBOL=demo_$(DEMO)
SOURCES := $(MODEL_FILES_$(DEMO))
OBJS := $(addprefix $(BUILD_DIR)/,$(SOURCES:.c=.o) demo_support.o boot_rv.o trap_rv.o context_rv.o sbi.o)

.DEFAULT_GOAL := all
.PHONY: all run clean

all: $(ELF)

$(OBJS): $(BUILD_SETTINGS)

$(LD_LLD):
	@mkdir -p $(@D)
	@test -n "$(LLD_REAL)" || { echo 'ld.lld or rust-lld is required' >&2; exit 1; }
	ln -sf "$(LLD_REAL)" $@

$(ELF): $(OBJS) $(ARCH_DIR)/link.ld $(LD_LLD)
	$(LD_LLD) -T $(ARCH_DIR)/link.ld --no-relax -o $@ $(OBJS)

$(BUILD_DIR)/%.o: %.c
	@mkdir -p $(@D)
	$(CLANG) $(MODEL_FLAGS) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/demo_support.o: $(SHARED)/demo.c
	@mkdir -p $(@D)
	$(CLANG) $(MODEL_FLAGS) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/sbi.o: $(ARCH_DIR)/sbi.c
	@mkdir -p $(@D)
	$(CLANG) $(MODEL_FLAGS) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/%_rv.o: $(ARCH_DIR)/%.S
	@mkdir -p $(@D)
	$(CLANG) $(MODEL_FLAGS) $(CFLAGS) -c $< -o $@

run: $(ELF)
	timeout $(QEMU_TIMEOUT) $(QEMU) $(QEMUFLAGS) -kernel $(ELF)

clean:
	rm -f $(ELF) $(LD_LLD) $(OBJS) $(OBJS:.o=.d)

-include $(OBJS:.o=.d)
