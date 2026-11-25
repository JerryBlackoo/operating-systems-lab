CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g -ffreestanding -fno-builtin
CPPFLAGS ?= -DMICRO_LOADER_HOST_TEST=1 -I. -I../boot -I../memory -I../objects -I../sched -I../arch/riscv -I../../shared

loader_test: loader.c loader_test.c loader.h elf.h ../memory/frame.c \
	../memory/as.c ../memory/memory.h ../objects/object.c ../objects/object.h
	$(CC) $(CPPFLAGS) $(CFLAGS) loader.c loader_test.c ../memory/frame.c \
		../memory/as.c ../objects/object.c -o $@

.PHONY: test clean
test: loader_test
	./loader_test

clean:
	rm -f loader_test