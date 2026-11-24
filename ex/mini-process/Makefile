BUILD := ../../mono/build/mini-process
IMAGE := $(BUILD)/boot.img

OBJCOPY ?= objcopy
QEMU ?= qemu-system-i386
ACCEL ?= tcg
CPU_MODEL ?=
GDB_PORT ?= 1234

CFLAGS := -m32 -Og -g -ffreestanding -fno-pie -fno-stack-protector \
	-fno-asynchronous-unwind-tables -fcf-protection=none -mgeneral-regs-only \
	-Wall -Wextra
QEMU_FLAGS := -machine pc,accel=$(ACCEL) $(if $(CPU_MODEL),-cpu $(CPU_MODEL)) -smp 1 -m 16 \
	-drive file="$(IMAGE)",format=raw,if=ide -boot order=c -snapshot \
	-display none -serial none -monitor none -debugcon stdio -no-reboot \
	-device isa-debug-exit,iobase=0xf4,iosize=0x04

.PHONY: all run debug
.DELETE_ON_ERROR:

all: $(IMAGE)

debug: QEMU_FLAGS += -S -gdb tcp:127.0.0.1:$(GDB_PORT)
run debug: $(IMAGE)
	@status=0; $(QEMU) $(QEMU_FLAGS) || status=$$?; test "$$status" -eq 33

$(BUILD):
	mkdir -p "$@"

$(BUILD)/boot.o: ../x86-boot/boot.S | $(BUILD)
	$(AS) --32 -o "$@" "$<"

$(BUILD)/stage2.o: load-kernel.S $(BUILD)/kernel.bin Makefile | $(BUILD)
	sectors=$$(( ($$(wc -c < "$(BUILD)/kernel.bin") + 511) / 512 )); \
	$(AS) --32 --defsym KERNEL_SECTORS=$$sectors -o "$@" "$<"

$(BUILD)/kernel-entry.o: ../x86-bios/kernel-entry.S | $(BUILD)
	$(AS) --32 -g -o "$@" "$<"

$(BUILD)/switch.o: switch.S | $(BUILD)
	$(AS) --32 -g -o "$@" "$<"

$(BUILD)/kernel.o: kernel.c Makefile | $(BUILD)
	$(CC) $(CFLAGS) -c -o "$@" "$<"

$(BUILD)/boot.elf: $(BUILD)/boot.o
	$(LD) -m elf_i386 -Ttext 0x7c00 -o "$@" "$<"

$(BUILD)/stage2.elf: $(BUILD)/stage2.o
	$(LD) -m elf_i386 -Ttext 0x7e00 -o "$@" "$<"

$(BUILD)/kernel.elf: kernel.ld $(BUILD)/kernel-entry.o $(BUILD)/switch.o $(BUILD)/kernel.o
	$(LD) -m elf_i386 -T "$<" -o "$@" $(filter %.o,$^)

$(BUILD)/%.bin: $(BUILD)/%.elf
	$(OBJCOPY) -O binary "$<" "$@"

$(IMAGE): $(BUILD)/boot.bin $(BUILD)/stage2.bin $(BUILD)/kernel.bin
	cat $^ > "$@.tmp"
	truncate -s %512 "$@.tmp"
	mv "$@.tmp" "$@"
