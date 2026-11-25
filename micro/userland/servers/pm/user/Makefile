include ../../../user-common.mk

NAME := pm
OBJS := $(BUILD)/pm.o $(BUILD)/pm_main.o $(BUILD)/lib_user_io.o $(BUILD)/lib_runtime.o

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/pm.o: pm.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/pm_main.o: pm_main.c
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