CLANG ?= clang
TARGET ?= riscv64-unknown-elf
CC := $(CLANG) --target=$(TARGET)
BUILD_DIR ?= ../../build/clock

CFLAGS := -std=c11 -ffreestanding -fno-builtin -fno-stack-protector -fno-pie \
          -fno-asynchronous-unwind-tables -fno-unwind-tables -nostdlib \
          -O2 -g -Wall -Wextra \
          -march=rv64imac_zicsr_zifencei -mabi=lp64 -mcmodel=medany
CPPFLAGS := -I.

OBJS := $(addprefix $(BUILD_DIR)/,timebase.o timer.o event.o)

.PHONY: all clean
all: $(OBJS)

$(BUILD_DIR)/%.o: %.c clock.h platform.h
	@mkdir -p $(BUILD_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -c $< -o $@

clean:
	rm -rf $(BUILD_DIR)
