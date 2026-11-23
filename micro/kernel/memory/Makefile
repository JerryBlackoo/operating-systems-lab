CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g -ffreestanding -fno-builtin
CPPFLAGS ?= -I. -I../objects

memory_test: frame.c as.c memory_test.c memory.h ../objects/object.c \
	../objects/object.h
	$(CC) $(CPPFLAGS) $(CFLAGS) frame.c as.c memory_test.c \
		../objects/object.c -o $@

.PHONY: test clean
test: memory_test
	./memory_test

clean:
	rm -f memory_test
