include ../../user-common.mk

NAME := heaptest2
OBJS := $(BUILD)/heaptest2.o $(BUILD)/heaptest2_main.o $(BUILD)/lib_user_io.o $(BUILD)/lib_runtime.o

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/heaptest2.o: heaptest2.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/heaptest2_main.o: ../../heaptest/user/heaptest_main.c
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