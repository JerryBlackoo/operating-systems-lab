CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g
CPPFLAGS ?= -I. -I../arch/riscv

fdt_test: fdt_initrd.c bootinfo.c fdt_test.c fdt_initrd.h bootinfo.h ../arch/riscv/types.h
	$(CC) $(CPPFLAGS) $(CFLAGS) fdt_initrd.c bootinfo.c fdt_test.c -o $@

.PHONY: test clean
test: fdt_test
	./fdt_test

clean:
	rm -f fdt_test
