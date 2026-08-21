SHELL := /bin/bash

RUSTUP_TOOLCHAIN := nightly-2025-01-18
CARGO_NET_OFFLINE := true
export RUSTUP_TOOLCHAIN CARGO_NET_OFFLINE

KERNEL_DIR := kernel

RV_PLATFORM := riscv64-qemu
RV_TARGET := riscv64imac-unknown-none-elf
RV_ELF := $(KERNEL_DIR)/target/$(RV_TARGET)/release/kernel

LA_PLATFORM := loongarch64-qemu
LA_TARGET := loongarch64-unknown-none
LA_ELF := $(KERNEL_DIR)/target/$(LA_TARGET)/release/kernel

all: kernel-rv kernel-la

$(KERNEL_DIR)/mount.img:
	dd if=/dev/zero of=$@ bs=1M count=16
	mkfs.ext4 -q -b 4096 -F -O ^metadata_csum_seed $@

kernel-rv: $(KERNEL_DIR)/mount.img
	$(MAKE) -C $(KERNEL_DIR) PLATFORM=$(RV_PLATFORM) build
	cp $(RV_ELF) $@

kernel-la: $(KERNEL_DIR)/mount.img
	$(MAKE) -C $(KERNEL_DIR) PLATFORM=$(LA_PLATFORM) build
	cp $(LA_ELF) $@

clean:
	$(MAKE) -C $(KERNEL_DIR) clean
	rm -f kernel-rv kernel-la $(KERNEL_DIR)/mount.img

.PHONY: all clean
