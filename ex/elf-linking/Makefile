CC ?= cc
AR ?= ar
BUILD ?= build
CFLAGS := -O0 -g -Wall -Wextra -Werror

.PHONY: all check trace
.DELETE_ON_ERROR:

all: $(BUILD)/static $(BUILD)/startup $(BUILD)/runtime

$(BUILD):
	mkdir -p "$@"

$(BUILD)/calc.o: calc.c | $(BUILD)
	$(CC) $(CFLAGS) -fPIC -c "$<" -o "$@"

$(BUILD)/libcalc.a: $(BUILD)/calc.o
	$(AR) rcs "$@" "$<"

$(BUILD)/libcalc.so: $(BUILD)/calc.o
	$(CC) -shared -Wl,-soname,libcalc.so -o "$@" "$<"

$(BUILD)/static: direct.c $(BUILD)/libcalc.a
	$(CC) $(CFLAGS) -no-pie -static -o "$@" direct.c "$(BUILD)/libcalc.a"

$(BUILD)/startup: direct.c $(BUILD)/libcalc.so
	$(CC) $(CFLAGS) -fPIE -pie -Wl,-z,now -o "$@" direct.c -L"$(BUILD)" -Wl,-rpath,'$$ORIGIN' -lcalc

$(BUILD)/runtime: demand.c $(BUILD)/libcalc.so
	$(CC) $(CFLAGS) -fPIE -pie -Wl,-z,now -o "$@" demand.c -Wl,-rpath,'$$ORIGIN' -ldl

check: all
	python3 verify.py "$(BUILD)"

trace: all
	bash trace.sh "$(BUILD)"
