CLANG   ?= clang
ARCH    ?= riscv
BOOT_MODULES ?= capability

include shared/modules.mk

comma := ,
empty :=
space := $(empty) $(empty)
BOOT_SELECTED := $(strip $(subst $(comma), ,$(BOOT_MODULES)))
ifeq ($(BOOT_SELECTED),all)
BOOT_SELECTED := $(BOOT_MODELS)
endif
BOOT_UNKNOWN := $(filter-out $(BOOT_MODELS),$(BOOT_SELECTED))
ifneq ($(BOOT_UNKNOWN),)
$(error Unknown BOOT_MODULES entry: $(BOOT_UNKNOWN). Run make list-modules)
endif
ifneq ($(words $(BOOT_SELECTED)),$(words $(sort $(BOOT_SELECTED))))
$(error BOOT_MODULES contains a duplicate module)
endif

ifeq ($(BOOT_SELECTED),capability)
BUILD_DIR ?= build/$(ARCH)
else
BOOT_PROFILE := $(if $(BOOT_SELECTED),$(subst $(space),-,$(BOOT_SELECTED)),boot-only)
BUILD_DIR ?= build/$(ARCH)-bootstrap/$(BOOT_PROFILE)
endif

KERNEL    := $(BUILD_DIR)/kernel.elf
BOOT_PLAN := $(BUILD_DIR)/bootstrap_plan.c

# 优先 ld.lld，其次 rust-lld（系统可能只装了后者）。
# rust-lld 通过 argv[0] 选择驱动，所以必须用 ld.lld 这个名字暴露它。
LLD_REAL ?= $(shell command -v ld.lld 2>/dev/null || \
            command -v rust-lld 2>/dev/null || \
            ls "$$HOME"/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -n 1)

LD_LLD := $(BUILD_DIR)/ld.lld

ifeq ($(ARCH),riscv)
CC_TARGET   := --target=riscv64-unknown-elf
ARCH_CFLAGS := -march=rv64imac_zicsr_zifencei -mabi=lp64 -mcmodel=medany
LDSCRIPT    := kernel/arch/riscv/link.ld
QEMU        ?= qemu-system-riscv64
QEMUFLAGS   ?= -machine virt -m 128M -smp 1 -nographic -monitor none -bios default
QEMU_TIMEOUT ?= 15s
endif

COMMON_CFLAGS := $(CC_TARGET) $(ARCH_CFLAGS) \
                 -ffreestanding -fno-builtin -fno-stack-protector \
                 -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
                 -mno-relax -msmall-data-limit=0 \
                 -nostdlib -O2 -g -Wall -Wextra -Werror -MD -MP \
                 -Ikernel -Ikernel/boot -Ishared \
                 -Ikernel/objects -Ikernel/memory -Ikernel/sched -Ikernel/ipc \
                 -Ikernel/capability -Iuserland/servers/pm -Iuserland/servers/sm \
                 -isystem kernel/arch/$(ARCH)

MODEL_SRCS := $(sort $(foreach module,$(BOOT_SELECTED), \
                  $(addprefix $(MODEL_DIR_$(module))/,$(MODEL_FILES_$(module))) \
                  $(MODEL_SUPPORT_$(module))))

C_SRCS := kernel/boot/boot.c \
          kernel/boot/bootinfo.c \
          kernel/boot/bootstrap.c \
          kernel/arch/riscv/sbi.c \
          shared/demo.c \
          $(filter %.c,$(MODEL_SRCS))

ASM_SRCS := kernel/arch/riscv/boot.S \
            kernel/arch/riscv/trap.S \
            kernel/arch/riscv/context.S \
            $(filter %.S,$(MODEL_SRCS))

# 汇编产物加 _asm 后缀，避免 trap.c 与 trap.S 撞同一个 .o
C_OBJS   := $(patsubst %.c,$(BUILD_DIR)/%.o,$(C_SRCS))
ASM_OBJS := $(patsubst %.S,$(BUILD_DIR)/%_asm.o,$(ASM_SRCS))
OBJS     := $(C_OBJS) $(ASM_OBJS) $(BUILD_DIR)/bootstrap_plan.o
DEPS     := $(OBJS:.o=.d)

.DEFAULT_GOAL := all
.PHONY: all run clean list-modules FORCE

all: $(KERNEL)

$(OBJS): Makefile shared/modules.mk

$(LD_LLD):
	@mkdir -p $(@D)
	@test -n "$(LLD_REAL)" || { echo 'ld.lld or rust-lld is required' >&2; exit 1; }
	ln -sf "$(LLD_REAL)" $@

$(BOOT_PLAN): FORCE
	@mkdir -p $(@D)
	@temp="$@.$$$$"; { \
		printf '%s\n' '#include "demo.h"'; \
		$(foreach module,$(BOOT_SELECTED),printf '%s\n' 'extern const struct demo_module demo_$(module);';) \
		printf '%s\n' 'const struct demo_module *const bootstrap_plan[] = {'; \
		$(foreach module,$(BOOT_SELECTED),printf '%s\n' '    &demo_$(module),';) \
		printf '%s\n' '    NULL' '};'; \
	} > "$$temp" && \
		if cmp -s "$$temp" "$@"; then rm "$$temp"; else mv "$$temp" "$@"; fi

$(BUILD_DIR)/bootstrap_plan.o: $(BOOT_PLAN)
	$(CLANG) $(COMMON_CFLAGS) -c $(BOOT_PLAN) -o $@

FORCE:

$(KERNEL): $(OBJS) $(LDSCRIPT) $(LD_LLD)
	@mkdir -p $(@D)
	$(LD_LLD) -T $(LDSCRIPT) --no-relax -o $@ $(OBJS)
	@echo "built: $@"
	@llvm-size $@ 2>/dev/null || true

$(BUILD_DIR)/%.o: %.c
	@mkdir -p $(@D)
	$(CLANG) $(COMMON_CFLAGS) -c $< -o $@

$(BUILD_DIR)/%_asm.o: %.S
	@mkdir -p $(@D)
	$(CLANG) $(COMMON_CFLAGS) -c $< -o $@

run: $(KERNEL)
	timeout $(QEMU_TIMEOUT) $(QEMU) $(QEMUFLAGS) -kernel $(KERNEL)

list-modules:
	@printf '%s\n' $(BOOT_MODELS)

clean:
	rm -f $(KERNEL) $(BOOT_PLAN) $(OBJS) $(DEPS)

-include $(DEPS)
