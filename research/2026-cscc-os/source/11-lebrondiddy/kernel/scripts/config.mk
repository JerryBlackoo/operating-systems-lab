export RUSTFLAGS := -Clink-arg=-no-pie
export LOG  := error
export CARGO_NET_OFFLINE := true

CONFIG_FILE  := byteos.yaml
PLATFORM     := 

ROOT_FS      := ext4

ifeq ($(PLATFORM),riscv64-qemu)
  TARGET := riscv64imac-unknown-none-elf
  ARCH := riscv64
  BOARD := qemu
  DRIVER := kvirtio
else ifeq ($(PLATFORM),riscv64-vf2)
  TARGET := riscv64imac-unknown-none-elf
  ARCH := riscv64
  BOARD := visionfive2
  DRIVER := kramdisk
else ifeq ($(PLATFORM),x86_64-qemu)
  TARGET := x86_64-unknown-none
  ARCH := x86_64
  BOARD := qemu
  DRIVER := kvirtio
else ifeq ($(PLATFORM),x86_64-generic)
  TARGET := x86_64-unknown-none
  ARCH := x86_64
  BOARD := qemu
  DRIVER := kramdisk
else ifeq ($(PLATFORM),aarch64-qemu)
  TARGET := aarch64-unknown-none-softfloat
  ARCH := aarch64
  BOARD := qemu
  DRIVER := kramdisk
else ifeq ($(PLATFORM),loongarch64-qemu)
  TARGET := loongarch64-unknown-none
  ARCH := loongarch64
  BOARD := qemu
  DRIVER := kvirtio
else ifeq ($(PLATFORM),loongarch64-2k1000)
  TARGET := loongarch64-unknown-none
  ARCH := loongarch64
  BOARD := 2k1000
  DRIVER := kramdisk
else ifeq ($(strip $(PLATFORM)),)
  $(error PLATFORM must be set; use riscv64-qemu or loongarch64-qemu)
else
  $(error unsupported PLATFORM "$(PLATFORM)")
endif

CONFIGS      := board="$(BOARD)" root_fs="$(ROOT_FS)" driver="$(DRIVER)"
RUSTFLAGS    += $(foreach cfg,$(CONFIGS), --cfg=$(cfg))
