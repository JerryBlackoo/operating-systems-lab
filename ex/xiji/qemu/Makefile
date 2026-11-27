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
	-Wall -Wextra -DXIJI_FREESTANDING -I.. -I.
QEMU_FLAGS := -machine pc,accel=$(ACCEL) $(if $(CPU_MODEL),-cpu $(CPU_MODEL)) -smp 1 -m 16 \
	-drive file="$(IMAGE)",format=raw,if=ide -boot order=c -snapshot \
	-display none -serial none -monitor none -debugcon stdio -no-reboot \
	-device isa-debug-exit,iobase=0xf4,iosize=0x04

EXP_SRCS := $(SRC)/exp01_process.c $(SRC)/exp02_sched.c $(SRC)/exp03_partition.c \
	$(SRC)/exp04_segment.c $(SRC)/exp05_paging.c $(SRC)/exp06_device.c $(SRC)/exp07_filesys.c

HW_SRCS := $(HW)/pic_pit.c $(HW)/idt.c $(HW)/paging.c $(HW)/ata.c $(HW)/hw_verify.c

.PHONY: all run debug clean
.DELETE_ON_ERROR:

clean:
	rm -rf "$(BUILD)"

all: $(IMAGE)

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

$(BUILD)/boot.o: ../../x86-boot/boot.S | $(BUILD)
	$(AS) --32 -o "$@" "$<"

$(BUILD)/stage2.o: load-kernel.S $(BUILD)/kernel.bin Makefile | $(BUILD)
	sectors=$$(( ($$(wc -c < "$(BUILD)/kernel.bin") + 511) / 512 )); \
	$(AS) --32 --defsym KERNEL_SECTORS=$$sectors -o "$@" "$<"

$(BUILD)/kernel-entry.o: kernel-entry.S | $(BUILD)
	$(AS) --32 -g -o "$@" "$<"

$(BUILD)/trap.o: $(HW)/trap.S | $(BUILD)
	$(AS) --32 -g -o "$@" "$<"

$(BUILD)/kernel.o: kernel.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/%.o: $(SRC)/%.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/pic_pit.o: $(HW)/pic_pit.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/idt.o: $(HW)/idt.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/paging.o: $(HW)/paging.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/ata.o: $(HW)/ata.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/hw_verify.o: $(HW)/hw_verify.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/boot.elf: $(BUILD)/boot.o
	$(LD) -m elf_i386 -Ttext 0x7c00 -o "$@" "$<"

$(BUILD)/stage2.elf: $(BUILD)/stage2.o
	$(LD) -m elf_i386 -Ttext 0x7e00 -o "$@" "$<"

EXP_OBJS := $(BUILD)/exp01_process.o $(BUILD)/exp02_sched.o $(BUILD)/exp03_partition.o \
	$(BUILD)/exp04_segment.o $(BUILD)/exp05_paging.o $(BUILD)/exp06_device.o $(BUILD)/exp07_filesys.o

HW_OBJS := $(BUILD)/trap.o $(BUILD)/pic_pit.o $(BUILD)/idt.o $(BUILD)/paging.o $(BUILD)/ata.o $(BUILD)/hw_verify.o

$(BUILD)/kernel.elf: kernel.ld $(BUILD)/kernel-entry.o $(BUILD)/kernel.o $(EXP_OBJS) $(HW_OBJS)
	$(LD) -m elf_i386 -T "$<" -o "$@" $(filter %.o,$^)

$(BUILD)/%.bin: $(BUILD)/%.elf
	$(OBJCOPY) -O binary "$<" "$@"

$(IMAGE): $(BUILD)/boot.bin $(BUILD)/stage2.bin $(BUILD)/kernel.bin
	cat $^ > "$@.tmp"
	truncate -s %512 "$@.tmp"
	truncate -s $$((512 * $(DISK_SECTORS))) "$@.tmp"
	mv "$@.tmp" "$@"