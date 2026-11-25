include ../user-common.mk

.PHONY: all clean
all: $(BUILD)/console.elf

OBJS_COMMON := $(BUILD)/stub.o $(BUILD)/stub_main.o $(BUILD)/lib_runtime.o

$(BUILD)/stub.o: stub.S
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/stub_main.o: stub_main.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(BUILD)/lib_%.o: $(MICRO_ROOT)/userland/libs/%.c
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

define link_elf
$(BUILD)/$(1).elf: $(OBJS_COMMON) $(USER_LD_SCRIPT) $(USER_LINKER)
	@mkdir -p $$(dir $$@)
	"$(USER_LINKER)" -T $(USER_LD_SCRIPT) --no-relax -o $$@ $(OBJS_COMMON)
endef

$(eval $(call link_elf,console))

clean:
	rm -f $(OBJS_COMMON) $(BUILD)/console.elf