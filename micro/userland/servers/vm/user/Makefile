include ../../../user-common.mk

CFLAGS += -I$(MICRO_ROOT)/userland/servers/vm

NAME := vm
OBJS := $(BUILD)/vm_entry.o $(BUILD)/vm_impl.o $(BUILD)/vm_main.o \
        $(BUILD)/lib_user_io.o $(BUILD)/lib_runtime.o

.PHONY: all clean
all: $(BUILD)/$(NAME).elf

$(BUILD)/vm_entry.o: vm.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/vm_impl.o: ../vm.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/vm_main.o: vm_main.c
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