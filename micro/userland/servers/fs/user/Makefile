include ../../../user-common.mk

NAME := fs
# A1：ext4 与 vfs 现在链进 U-mode 服务（内核只保留块驱动）。
#
# 这两个模块是纯策略代码，不碰硬件也不依赖内核符号；它们唯一的外部
# 依赖是 ext4_blk_io / vfs_port 两个回调端口，本目录在 fs_main.c 里
# 把它们接到 ABI_BLOCK_READ 上。
OBJS := $(BUILD)/fs.o $(BUILD)/fs_main.o \
        $(BUILD)/fs_ext4.o $(BUILD)/fs_vfs.o \
        $(BUILD)/lib_user_io.o $(BUILD)/lib_runtime.o

CFLAGS += -I$(MICRO_ROOT)/userland/servers/fs/ext4 \
          -I$(MICRO_ROOT)/userland/servers/vfs

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/fs.o: fs.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/fs_main.o: fs_main.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/fs_ext4.o: ../../fs/ext4/ext4.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/fs_vfs.o: ../../vfs/vfs.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/lib_%.o: $(MICRO_ROOT)/userland/libs/%.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/$(NAME).elf: $(OBJS) $(USER_LD_SCRIPT) $(USER_LINKER)
	@mkdir -p $(dir $@)
	"$(USER_LINKER)" -T $(USER_LD_SCRIPT) --no-relax -o $@ $(OBJS)

clean:
	rm -f $(OBJS) $(BUILD)/$(NAME).elf
