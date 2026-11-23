CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g -ffreestanding -fno-builtin
CPPFLAGS ?= -I.

objects_test: object.c object_test.c object.h
	$(CC) $(CPPFLAGS) $(CFLAGS) object.c object_test.c -o $@

.PHONY: test clean
test: objects_test
	./objects_test

clean:
	rm -f objects_test
