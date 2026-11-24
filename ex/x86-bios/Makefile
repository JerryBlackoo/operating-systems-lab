BUILD := ../../mono/build/x86-bios
IMAGE := $(BUILD)/boot.img
PARTITION_LBA := 2048
PARTITION_SECTORS := 2048
IMAGE_SECTORS := 8192

OBJCOPY ?= objcopy
QEMU ?= qemu-system-i386
BIOS ?= /usr/share/seabios/bios-256k.bin
GDB_PORT ?= 1234
QEMU_FLAGS := -machine pc,accel=tcg -bios "$(BIOS)" \
	-drive file="$(IMAGE)",format=raw,if=ide -boot order=c -snapshot \
	-display none -serial none -monitor none -debugcon stdio

.PHONY: all run debug
.DELETE_ON_ERROR:

all: $(IMAGE)

run: $(IMAGE)
	$(QEMU) $(QEMU_FLAGS)

debug: $(IMAGE)
	$(QEMU) $(QEMU_FLAGS) -S -gdb tcp:127.0.0.1:$(GDB_PORT)

$(BUILD):
	mkdir -p "$@"

$(BUILD)/mbr.o: mbr.S Makefile | $(BUILD)
	$(AS) --32 --defsym PARTITION_LBA=$(PARTITION_LBA) \
		--defsym PARTITION_SECTORS=$(PARTITION_SECTORS) -o "$@" "$<"

$(BUILD)/pbr.o: pbr.S | $(BUILD)
	$(AS) --32 -o "$@" "$<"

$(BUILD)/kernel-entry.o: kernel-entry.S | $(BUILD)
	$(AS) --32 -o "$@" "$<"

$(BUILD)/kernel-main.o: kernel.c Makefile | $(BUILD)
	$(CC) -m32 -Os -ffreestanding -fno-pie -fno-stack-protector \
		-fno-asynchronous-unwind-tables -fcf-protection=none -Wall -Wextra \
		-c -o "$@" "$<"

$(BUILD)/mbr.elf: $(BUILD)/mbr.o
	$(LD) -m elf_i386 -Ttext=0x0600 -o "$@" "$<"

$(BUILD)/pbr.elf: $(BUILD)/pbr.o
	$(LD) -m elf_i386 -Ttext=0x7c00 -o "$@" "$<"

$(BUILD)/kernel.elf: kernel.ld $(BUILD)/kernel-entry.o $(BUILD)/kernel-main.o
	$(LD) -m elf_i386 -T "$<" -o "$@" $(filter %.o,$^)

$(BUILD)/%.bin: $(BUILD)/%.elf
	$(OBJCOPY) -O binary "$<" "$@"

$(IMAGE): $(BUILD)/mbr.bin $(BUILD)/pbr.bin $(BUILD)/kernel.bin Makefile
	test "$(PARTITION_LBA)" -ge 1
	test "$(PARTITION_SECTORS)" -ge 2
	test "$$(( $(PARTITION_LBA) + $(PARTITION_SECTORS) ))" -le "$(IMAGE_SECTORS)"
	truncate -s 0 "$@.tmp"
	truncate -s "$$(( $(IMAGE_SECTORS) * 512 ))" "$@.tmp"
	dd if="$(BUILD)/mbr.bin" of="$@.tmp" bs=512 conv=notrunc status=none
	dd if="$(BUILD)/pbr.bin" of="$@.tmp" bs=512 seek=$(PARTITION_LBA) conv=notrunc status=none
	dd if="$(BUILD)/kernel.bin" of="$@.tmp" bs=512 seek="$$(( $(PARTITION_LBA) + 1 ))" conv=notrunc status=none
	mv "$@.tmp" "$@"
