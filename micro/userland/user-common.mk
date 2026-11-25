CLANG ?= clang

MICRO_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
BUILD ?= $(MICRO_ROOT)/build/user
LLD_REAL ?= $(shell command -v ld.lld 2>/dev/null || command -v rust-lld 2>/dev/null || ls "$$HOME"/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -n 1)
USER_LINKER ?= $(BUILD)/ld.lld

$(USER_LINKER):
	@mkdir -p $(dir $@)
	@test -n "$(LLD_REAL)" || { echo 'ld.lld or rust-lld is required' >&2; exit 1; }
	ln -sf "$(LLD_REAL)" $@

CFLAGS := --target=riscv64-unknown-elf -march=rv64imac_zicsr_zifencei -mabi=lp64 \
          -mcmodel=medany -msmall-data-limit=0 -ffreestanding -fno-builtin -nostdlib \
          -O2 -g -Wall -Wextra -Werror \
          -I$(MICRO_ROOT)/shared -I$(MICRO_ROOT)/userland/libs

USER_LD_SCRIPT ?= $(MICRO_ROOT)/userland/init/user/user.ld
LIB_SRCS := $(MICRO_ROOT)/userland/libs/user_io.c $(MICRO_ROOT)/userland/libs/runtime.c
.DEFAULT_GOAL := all