BUILD := ../../../mono/build/xiji-qemu
TRACE_DIR := ../build/traces
IMAGE := $(BUILD)/boot.img
SRC := ../src
HW := hw

OBJCOPY ?= objcopy
QEMU ?= qemu-system-i386
ACCEL ?= tcg
CPU_MODEL ?=
GDB_PORT ?= 1234
DISK_SECTORS ?= 128

CFLAGS := -m32 -Og -g -ffreestanding -fno-pie -fno-stack-protector \
	-fno-asynchronous-unwind-tables -fcf-protection=none -mgeneral-regs-only \
	-Wall -Wextra -Wshadow -Wstrict-prototypes -Wmissing-prototypes \
	-DXIJI_FREESTANDING -I.. -I.
LDFLAGS := -m elf_i386

# 头文件依赖：改 .h 必须触发重编，否则会静默用旧二进制
DEPFLAGS := -MMD -MP
DEPS :=

QEMU_FLAGS := -machine pc,accel=$(ACCEL) $(if $(CPU_MODEL),-cpu $(CPU_MODEL)) -smp 1 -m 16 \
	-drive file="$(IMAGE)",format=raw,if=ide -boot order=c -snapshot \
	-display none -serial none -monitor none -debugcon stdio -no-reboot \
	-device isa-debug-exit,iobase=0xf4,iosize=0x04

EXP_NAMES := exp01_process exp02_sched exp03_partition exp04_segment \
	exp05_paging exp06_device exp07_filesys
HW_NAMES := pic_pit idt paging ata hw_verify

EXP_OBJS := $(addprefix $(BUILD)/,$(addsuffix .o,$(EXP_NAMES)))
HW_OBJS := $(addprefix $(BUILD)/,$(addsuffix .o,$(HW_NAMES))) $(BUILD)/trap.o
DEPS := $(EXP_OBJS:.o=.d) $(HW_OBJS:.o=.d) $(BUILD)/kernel.d

.PHONY: all run debug clean
.DELETE_ON_ERROR:

all: $(IMAGE)

clean:
	rm -rf "$(BUILD)"

debug: QEMU_FLAGS += -S -gdb tcp:127.0.0.1:$(GDB_PORT)
run debug: $(IMAGE)
	@mkdir -p "$(TRACE_DIR)"
ifdef QEMU_LOG
	@bash -euo pipefail -c 'status=0; $(QEMU) $(QEMU_FLAGS) 2>&1 | tee "$(TRACE_DIR)/qemu.stdout" || status=$$?; test "$$status" -eq 33'
else
	@status=0; $(QEMU) $(QEMU_FLAGS) || status=$$?; test "$$status" -eq 33
endif

$(BUILD):
	mkdir -p "$@"

# --- 引导 ---

$(BUILD)/boot.o: ../../x86-boot/boot.S | $(BUILD)
	$(AS) --32 -o "$@" "$<"

$(BUILD)/stage2.o: load-kernel.S $(BUILD)/kernel.bin Makefile | $(BUILD)
	sectors=$$(( ($$(wc -c < "$(BUILD)/kernel.bin") + 511) / 512 )); \
	$(AS) --32 --defsym KERNEL_SECTORS=$$sectors --defsym KERNEL_BASE=0x20000 -o "$@" "$<"

$(BUILD)/kernel-entry.o: kernel-entry.S | $(BUILD)
	$(AS) --32 -g -o "$@" "$<"

$(BUILD)/trap.o: $(HW)/trap.S | $(BUILD)
	$(AS) --32 -g -o "$@" "$<"

# --- C 源文件 ---

$(BUILD)/kernel.o: kernel.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) $(DEPFLAGS) -c -o "$@" "$<"

$(BUILD)/%.o: $(SRC)/%.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) $(DEPFLAGS) -c -o "$@" "$<"

$(BUILD)/%.o: $(HW)/%.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) $(DEPFLAGS) -c -o "$@" "$<"

# --- 链接 ---

$(BUILD)/boot.elf: $(BUILD)/boot.o
	$(LD) $(LDFLAGS) -Ttext 0x7c00 -o "$@" "$<"

$(BUILD)/stage2.elf: $(BUILD)/stage2.o
	$(LD) $(LDFLAGS) -Ttext 0x7e00 -o "$@" "$<"

$(BUILD)/kernel.elf: kernel.ld $(BUILD)/kernel-entry.o $(BUILD)/kernel.o $(EXP_OBJS) $(HW_OBJS)
	$(LD) $(LDFLAGS) -T "$<" -o "$@" $(filter %.o,$^)

$(BUILD)/%.bin: $(BUILD)/%.elf
	$(OBJCOPY) -O binary "$<" "$@"

$(IMAGE): $(BUILD)/boot.bin $(BUILD)/stage2.bin $(BUILD)/kernel.bin
	cat $^ > "$@.tmp"
	truncate -s %512 "$@.tmp"
	truncate -s $$((512 * $(DISK_SECTORS))) "$@.tmp"
	mv "$@.tmp" "$@"

-include $(DEPS)
