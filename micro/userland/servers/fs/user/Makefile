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

# 头文件依赖。这里必须显式列出：`struct vfs` / `struct ext4` 是跨编译单元的
# 布局契约，只依赖 .c 会让「改了头文件但没重编 fs_main.o」的构建通过链接，
# 两个目标文件对结构体大小的理解不一致，直接把静态实例写坏。
VFS_HDRS := $(MICRO_ROOT)/userland/servers/vfs/vfs.h
EXT4_HDRS := $(MICRO_ROOT)/userland/servers/fs/ext4/ext4.h

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/fs.o: fs.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/fs_main.o: fs_main.c $(VFS_HDRS) $(EXT4_HDRS)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/fs_ext4.o: ../../fs/ext4/ext4.c $(EXT4_HDRS)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/fs_vfs.o: ../../vfs/vfs.c $(VFS_HDRS)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/lib_%.o: $(MICRO_ROOT)/userland/libs/%.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/$(NAME).elf: $(OBJS) $(USER_LD_SCRIPT) $(USER_LINKER)
	@mkdir -p $(dir $@)
	"$(USER_LINKER)" -T $(USER_LD_SCRIPT) --no-relax -o $@ $(OBJS)

# 编译器生成的 -MD 依赖（头文件改动会触发重编）。
-include $(OBJS:.o=.d)

clean:
	rm -f $(OBJS) $(BUILD)/$(NAME).elf $(OBJS:.o=.d)
