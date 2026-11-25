include ../../user-common.mk

NAME := init
SRCS := init.S init_main.c

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/%.o: %.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/%.o: %.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/lib_%.o: $(MICRO_ROOT)/userland/libs/%.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

# 默认链 init_main.o（plan=3）；实验 PM-only 见 init_main_m6.o
OBJS := $(BUILD)/init.o $(BUILD)/init_main.o $(BUILD)/lib_user_io.o $(BUILD)/lib_runtime.o

$(BUILD)/$(NAME).elf: $(OBJS) $(USER_LD_SCRIPT) $(USER_LINKER)
	@mkdir -p $(dir $@)
	"$(USER_LINKER)" -T $(USER_LD_SCRIPT) --no-relax -o $@ $(OBJS)

clean:
	rm -f $(OBJS) $(BUILD)/$(NAME).elf