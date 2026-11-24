CLANG ?= clang
ARCH ?= riscv

BUILD_DIR := build/$(ARCH)
KERNEL := $(BUILD_DIR)/kernel.elf

# 优先使用系统的 ld.lld，其次 rust-lld，最后 rustup 工具链自带的 rust-lld
# （系统 rustc 的 sysroot 可能不包含它）。
LLD ?= $(shell command -v ld.lld 2>/dev/null || \
          command -v rust-lld 2>/dev/null || \
          ls $$HOME/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -n 1)

# clang 根据 argv[0] 选择 lld 驱动，因此用 ld.lld 这个名字暴露链接器。
TOOLCHAIN_DIR := build/toolchain
LD_LLD := $(TOOLCHAIN_DIR)/ld.lld

LLVM_OBJCOPY ?= llvm-objcopy
USER_DIR := $(BUILD_DIR)/userland
USER_PROGS := hello forktest faulttest init sh
USER_ELFS := $(addprefix $(USER_DIR)/,$(addsuffix .elf,$(USER_PROGS)))
USER_BINS := $(addprefix $(USER_DIR)/,$(addsuffix .bin,$(USER_PROGS)))

COMMON_CFLAGS := -ffreestanding -fno-builtin -fno-stack-protector \
                 -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
                 -nostdlib -O2 -g -Wall -Wextra -Ikernel -I.

ifeq ($(ARCH),riscv)
ARCH_CFLAGS := --target=riscv64-unknown-elf \
               -march=rv64imac_zicsr_zifencei -mabi=lp64 -mcmodel=medany
LDSCRIPT := kernel/arch/riscv/linker.ld
LDFLAGS_EXTRA := -Wl,--no-relax
C_SRCS := kernel/boot/boot.c \
          kernel/lib/string.c \
          kernel/lib/kprintf.c \
          kernel/proc/proc.c \
          kernel/proc/thread.c \
          kernel/proc/sched.c \
          kernel/proc/user.c \
          kernel/tests/sched_demo.c \
          kernel/tests/prodcons.c \
          kernel/sync/sem.c \
          kernel/sync/mutex.c \
          kernel/sync/banker.c \
          kernel/syscall/syscall.c \
          kernel/mm/vm.c \
          kernel/mm/trap_vm.c \
          kernel/fs/bio.c \
          kernel/fs/fs.c \
          kernel/fs/disk_sched.c \
          kernel/dev/device.c \
          kernel/arch/riscv/vm.c \
          kernel/arch/riscv/platform.c \
          kernel/arch/riscv/uart.c \
          kernel/arch/riscv/sbi.c \
          kernel/interrupt/trap.c \
          kernel/exception/exception.c \
          kernel/clock/clock.c \
          kernel/memory/pmm.c
ASM_SRCS := kernel/arch/riscv/boot.S kernel/arch/riscv/trap.S kernel/arch/riscv/switch.S
USER_ARCH_CFLAGS := --target=riscv64-unknown-elf \
                    -march=rv64imac_zicsr_zifencei -mabi=lp64 -mcmodel=medany
USER_START := userland/start_riscv.S
QEMU ?= qemu-system-riscv64
QEMUFLAGS ?= -machine virt -m 128M -nographic -bios default
else ifeq ($(ARCH),arm)
ARCH_CFLAGS := --target=arm-none-eabi -mcpu=cortex-a9 -march=armv7-a -marm \
               -mfloat-abi=soft -fno-vectorize -fno-slp-vectorize -DPLATFORM_ZYNQ=1
LDSCRIPT := kernel/arch/arm/linker.ld
LDFLAGS_EXTRA :=
C_SRCS := kernel/boot/boot.c \
          kernel/lib/string.c \
          kernel/lib/kprintf.c \
          kernel/proc/proc.c \
          kernel/proc/thread.c \
          kernel/proc/sched.c \
          kernel/proc/user.c \
          kernel/tests/sched_demo.c \
          kernel/tests/prodcons.c \
          kernel/sync/sem.c \
          kernel/sync/mutex.c \
          kernel/sync/banker.c \
          kernel/syscall/syscall.c \
          kernel/mm/vm.c \
          kernel/mm/trap_vm.c \
          kernel/fs/bio.c \
          kernel/fs/fs.c \
          kernel/fs/disk_sched.c \
          kernel/dev/device.c \
          kernel/arch/arm/vm.c \
          kernel/arch/arm/platform.c \
          kernel/arch/arm/uart.c \
          kernel/arch/arm/clock.c \
          kernel/arch/arm/irq.c \
          kernel/arch/arm/libgcc.c \
          kernel/memory/pmm.c
ASM_SRCS := kernel/arch/arm/boot.S kernel/arch/arm/vectors.S kernel/arch/arm/switch.S \
            kernel/arch/arm/eabi.S
USER_ARCH_CFLAGS := --target=arm-none-eabi -mcpu=cortex-a9 -march=armv7-a -marm \
                    -mfloat-abi=soft -DPLATFORM_ZYNQ=1
USER_START := userland/start_arm.S
QEMU ?= qemu-system-arm
QEMUFLAGS ?= -M xilinx-zynq-a9 -m 512M -nographic
else
$(error unsupported ARCH=$(ARCH); use ARCH=riscv or ARCH=arm)
endif

CFLAGS := $(ARCH_CFLAGS) $(COMMON_CFLAGS)
DEPFLAGS := -MMD -MP
LDFLAGS := -nostdlib -Wl,-T,$(LDSCRIPT) -Wl,-Map,$(BUILD_DIR)/kernel.map $(LDFLAGS_EXTRA)

USER_CFLAGS := $(USER_ARCH_CFLAGS) -ffreestanding -fno-builtin -fno-pie \
               -fno-stack-protector -nostdlib -O2 -g -Wall -Wextra -I.

OBJS := $(patsubst %.c,$(BUILD_DIR)/%.o,$(C_SRCS)) \
        $(patsubst %.S,$(BUILD_DIR)/%.o,$(ASM_SRCS)) \
        $(BUILD_DIR)/userland_blob.o

all: $(KERNEL)

$(LD_LLD):
	@test -n "$(LLD)" || { echo "error: need ld.lld or rust-lld (or a Rust toolchain)"; exit 1; }
	@mkdir -p $(dir $@)
	@ln -sf $(LLD) $@

$(KERNEL): $(OBJS) $(LD_LLD) $(LDSCRIPT)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) $(LDFLAGS) --ld-path=$(LD_LLD) -fuse-ld=lld $(OBJS) -o $@
	@echo "built $@ (ARCH=$(ARCH))"

$(BUILD_DIR)/%.o: %.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) $(DEPFLAGS) -c $< -o $@

$(BUILD_DIR)/%.o: %.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

# 头文件依赖：避免修改 struct/宏后只重编部分目标文件，导致结构体布局不一致。
-include $(OBJS:.o=.d)

define USER_PROG_RULES
$(USER_DIR)/$(1).elf: $(USER_START) userland/lib.c userland/lib.h \
                      shared/abi/syscall.h userland/$(1)/$(1).c userland/link.ld $(LD_LLD)
	@mkdir -p $$(dir $$@)
	$(CLANG) $(USER_CFLAGS) -Wl,-T,userland/link.ld $(LDFLAGS_EXTRA) \
	    --ld-path=$(LD_LLD) -fuse-ld=lld $(USER_START) userland/lib.c \
	    userland/$(1)/$(1).c -o $$@

# .bss 默认是 NOBITS，flat 二进制会丢弃它；标记为 load/contents 后
# 镜像按零填充，内核 load_image 才能把 BSS 区间一起映射。
$(USER_DIR)/$(1).bin: $(USER_DIR)/$(1).elf
	$(LLVM_OBJCOPY) -O binary --set-section-flags .bss=alloc,load,contents $$< $$@
endef

$(foreach prog,$(USER_PROGS),$(eval $(call USER_PROG_RULES,$(prog))))

$(BUILD_DIR)/userland_blob.S: $(USER_BINS)
	@mkdir -p $(dir $@)
	@printf '.section .user,"a"\n' > $@
	@for n in $(USER_PROGS); do \
	    printf '.global _user_%s_start\n_user_%s_start:\n.incbin "%s/%s.bin"\n.global _user_%s_end\n_user_%s_end:\n' \
	        $$n $$n $(USER_DIR) $$n $$n $$n >> $@; \
	done

$(BUILD_DIR)/userland_blob.o: $(BUILD_DIR)/userland_blob.S
	$(CLANG) $(CFLAGS) -c $< -o $@

run: $(KERNEL)
	$(QEMU) $(QEMUFLAGS) -kernel $(KERNEL)

fs-image:
	python3 tools/mkfs.py build/ramdisk.img

.PHONY: fs-image

ifeq ($(ARCH),arm)
bin: $(KERNEL)
	$(LLVM_OBJCOPY) -O binary $(KERNEL) $(BUILD_DIR)/kernel.bin
	@echo "built $(BUILD_DIR)/kernel.bin"
.PHONY: bin
endif

clean:
	rm -rf build

.PHONY: all run clean
