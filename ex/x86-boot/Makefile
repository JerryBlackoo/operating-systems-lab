AS = as
LD = ld
OBJCOPY = objcopy

all: boot.img

boot.o: boot.S
	$(AS) --32 -o $@ $<

stage2.o: stage2.S
	$(AS) --32 -o $@ $<

boot.elf: boot.o
	$(LD) -m elf_i386 -Ttext 0x7C00 -o $@ $<

stage2.elf: stage2.o
	$(LD) -m elf_i386 -Ttext 0x7E00 -o $@ $<

boot.bin: boot.elf
	$(OBJCOPY) -O binary $< $@

stage2.bin: stage2.elf
	$(OBJCOPY) -O binary $< $@

boot.img: boot.bin stage2.bin
	cat boot.bin stage2.bin > $@
	@echo "--- boot.img ---"
	@ls -l boot.img
	@xxd -s 510 -l 2 boot.img
	@xxd -s 1022 -l 2 boot.img

run: boot.img
	qemu-system-x86_64 -drive format=raw,file=boot.img -m 16 \
		-display none -debugcon stdio -no-reboot

clean:
	rm -f *.o *.elf *.bin boot.img

.PHONY: all run clean
