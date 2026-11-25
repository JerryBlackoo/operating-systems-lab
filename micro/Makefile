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
FS_IMAGE    ?= build/micro-root.img
FS_IMAGE_RUN ?= build/micro-root-run.img
QEMUFLAGS   ?= -machine virt -m 128M -smp 1 -nographic -monitor none -bios default
SYSTEM_QEMUFLAGS ?= $(QEMUFLAGS) \
                    -drive file=$(FS_IMAGE_RUN),if=none,format=raw,readonly=on,id=d0 \
                    -device virtio-blk-device,drive=d0
QEMU_TIMEOUT ?= 45s
endif

COMMON_CFLAGS := $(CC_TARGET) $(ARCH_CFLAGS) \
                 -ffreestanding -fno-builtin -fno-stack-protector \
                 -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
                 -mno-relax -msmall-data-limit=0 \
                 -nostdlib -O2 -g -Wall -Wextra -Werror -MD -MP \
                 -Ikernel -Ikernel/boot -Ishared \
                 -Ikernel/objects -Ikernel/memory -Ikernel/sched -Ikernel/ipc \
                 -Ikernel/capability -Ikernel/loader -Ikernel/syscall \
                 -Ikernel/interrupt -Ikernel/console -Ikernel/storage \
                 -Iuserland/servers/block -Iuserland/servers/fs/ext4 \
                 -Iuserland/servers/vfs \
                 -Iuserland/servers/drivers/storage/virtio_blk \
                 -Iuserland/servers/pm -Iuserland/servers/sm \
                 -isystem kernel/arch/$(ARCH)

MODEL_SRCS := $(sort $(foreach module,$(BOOT_SELECTED), \
                  $(addprefix $(MODEL_DIR_$(module))/,$(MODEL_FILES_$(module))) \
                  $(MODEL_SUPPORT_$(module))))

C_SRCS := kernel/boot/boot.c \
          kernel/boot/bootinfo.c \
          kernel/boot/fdt_initrd.c \
          kernel/boot/bootstrap.c \
          kernel/arch/riscv/sbi.c \
          shared/demo.c \
          $(filter %.c,$(MODEL_SRCS))

# 默认 capability 镜像在正式 bootstrap 上跑一次 timer 抢占冒烟测试。
# 其它组合不链接这个文件，避免改变各模块 demo 的既有验收语义。
ifeq ($(BOOT_SELECTED),capability)
C_SRCS += kernel/boot/preempt_probe.c
C_SRCS += kernel/boot/kernel_init.c
C_SRCS += kernel/boot/resident_trap.c
C_SRCS += kernel/boot/service_boot.c
C_SRCS += kernel/boot/kernel_spawn.c
C_SRCS += kernel/loader/loader.c
C_SRCS += kernel/syscall/user_backend.c
C_SRCS += kernel/capability/cap_endpoint.c
C_SRCS += kernel/capability/cap_as.c
C_SRCS += kernel/arch/riscv/plic.c
C_SRCS += kernel/interrupt/irq.c
C_SRCS += kernel/console/console.c
C_SRCS += kernel/storage/sha256.c
C_SRCS += kernel/storage/storage_bringup.c
C_SRCS += userland/servers/block/block.c
C_SRCS += userland/servers/drivers/storage/virtio_blk/virtio_blk.c
C_SRCS += kernel/pager/pager.c
# A1：ext4 与 vfs 不再链进内核——它们现在编译进 U-mode `fs` 服务
# （见 userland/servers/fs/user/Makefile）。内核只保留块驱动，
# 通过 ABI_BLOCK_READ/ABI_BLOCK_INFO 暴露原始块访问。
COMMON_CFLAGS += -DMICRO_POST_BOOT_RESIDENT=1
# resident 下的 ABI_IPC_CALL 默认走异步路径（ep_call_open/deliver +
# syscall_wait_park/syscall_resume）：发起线程在 trap 里阻塞，由 ep_reply
# 直接写回调用帧，不再让调用者在内核栈上同步等待。
#
# 同步实现（ep_call，在内核里 sched 等待）保留为回退：
#   make MICRO_SYSCALL_ASYNC_IPC=0
MICRO_SYSCALL_ASYNC_IPC ?= 1
ifeq ($(MICRO_SYSCALL_ASYNC_IPC),1)
COMMON_CFLAGS += -DMICRO_SYSCALL_ASYNC_IPC=1
endif
USER_INIT  := build/user/init.elf
USER_SM    := build/user/sm.elf
USER_PM    := build/user/pm.elf
USER_FS    := build/user/fs.elf
USER_CONSOLE := build/user/console.elf
USER_SHELL := build/user/shell.elf
USER_VM    := build/user/vm.elf
USER_HEAPTEST := build/user/heaptest.elf
USER_HEAPTEST2 := build/user/heaptest2.elf
INITRD   := build/user/initrd.img
endif

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
.PHONY: all run clean list-modules check user initrd fs-image demo FORCE

all: $(KERNEL)

# 影响代码生成的开关不能只靠命令行变量：改 $(MICRO_SYSCALL_ASYNC_IPC) 时
# make 看不到任何依赖变化，会复用上一次开关下的 .o，导致镜像与语义不符。
CONFIG_STAMP := $(BUILD_DIR)/.config
CONFIG_LINES := ARCH=$(ARCH) ASYNC_IPC=$(MICRO_SYSCALL_ASYNC_IPC) \
                RESIDENT=$(MICRO_POST_BOOT_RESIDENT) MODULES=$(BOOT_SELECTED)

$(CONFIG_STAMP): FORCE
	@mkdir -p $(@D)
	@printf '%s\n' $(CONFIG_LINES) > $@.tmp; \
	if cmp -s $@.tmp $@; then rm -f $@.tmp; else mv $@.tmp $@; fi

$(OBJS): Makefile shared/modules.mk $(CONFIG_STAMP)

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

user:
	$(MAKE) -C userland/init/user
	$(MAKE) -C userland/servers/sm/user
	$(MAKE) -C userland/servers/pm/user
	$(MAKE) -C userland/servers/fs/user
	$(MAKE) -C userland/servers/console/user
	$(MAKE) -C userland/shell/user
	$(MAKE) -C userland/servers/vm/user
	$(MAKE) -C userland/heaptest/user
	$(MAKE) -C userland/heaptest2/user

initrd: user
	python3 tools/mkinitrd.py $(INITRD) \
		$(USER_INIT) $(USER_SM) $(USER_PM) $(USER_FS) $(USER_CONSOLE) $(USER_SHELL) \
		$(USER_VM) $(USER_HEAPTEST) $(USER_HEAPTEST2)

fs-image:
	python3 tools/mkext4image.py $(FS_IMAGE)

$(FS_IMAGE_RUN): $(FS_IMAGE)
	cp -f $(FS_IMAGE) $(FS_IMAGE_RUN)

ifeq ($(BOOT_SELECTED),capability)
run: fs-image user initrd $(KERNEL) $(FS_IMAGE_RUN)
	timeout $(QEMU_TIMEOUT) $(QEMU) $(SYSTEM_QEMUFLAGS) -initrd $(INITRD) -kernel $(KERNEL)
else
run: $(KERNEL)
	timeout $(QEMU_TIMEOUT) $(QEMU) $(QEMUFLAGS) -kernel $(KERNEL)
endif

list-modules:
	@printf '%s\n' $(BOOT_MODELS)

demo:
	python3 tools/demo.sh

# M0：host 自测 + 13 个单模块镜像 + 整内核清单（见 docs/goals/M0-baseline-check.md）
# 末尾追加 M7 常驻端到端：demo.sh 会构建默认 capability 镜像，用真实 stdin
# 喂 help/cat/ps/echo/poweroff 并校验 motd SHA。它覆盖 make check 里其它
# 目标不执行的 loader/initrd/virtio/ext4/pager/console IRQ 路径。
check:
	@set -e; \
	bash tools/verify_capability_wiring.sh; \
	echo '[check] host: boot objects memory sched ipc interrupt loader'; \
	$(MAKE) -C kernel/boot test; \
	$(MAKE) -C kernel/objects test; \
	$(MAKE) -C kernel/memory test; \
	$(MAKE) -C kernel/sched test; \
	$(MAKE) -C kernel/ipc test; \
	$(MAKE) -C kernel/interrupt test; \
	$(MAKE) -C kernel/loader test; \
	echo '[check] standalone module ELFs'; \
	$(MAKE) -C shared check; \
	echo '[check] every module boots in QEMU (20 modules, one by one)'; \
	python3 tools/modules_qemu.py; \
	echo '[check] full kernel BOOT_MODULES=all'; \
	$(MAKE) BOOT_MODULES=all run; \
	echo '[check] vm policy (region/resident slot reuse)'; \
	$(MAKE) -C userland/servers/vm run; \
	echo '[check] resident M7 end-to-end'; \
	python3 tools/demo.sh; \
	echo '[check] resident interactive smoke (command output integrity)'; \
	python3 tools/shell_smoke.py; \
	echo '[check] resident long session (IPC resource reuse)'; \
	python3 tools/shell_stress.py 20

clean:
	rm -f $(KERNEL) $(BOOT_PLAN) $(OBJS) $(DEPS) $(CONFIG_STAMP)

-include $(DEPS)
