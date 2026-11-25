include ../../../user-common.mk

NAME := sm
OBJS := $(BUILD)/sm.o $(BUILD)/sm_main.o $(BUILD)/lib_user_io.o $(BUILD)/lib_runtime.o

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/sm.o: sm.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/sm_main.o: sm_main.c
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