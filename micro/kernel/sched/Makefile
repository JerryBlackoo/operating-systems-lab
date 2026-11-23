CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g -ffreestanding -fno-builtin
CPPFLAGS ?= -I. -I../objects -I../memory -DSCHED_HOST_TEST

sched_test: sched.c sched_test.c sched.h ../objects/object.c \
	../objects/object.h ../memory/frame.c ../memory/as.c ../memory/memory.h
	$(CC) $(CPPFLAGS) $(CFLAGS) sched.c sched_test.c \
		../objects/object.c ../memory/frame.c ../memory/as.c -o $@

.PHONY: test clean
test: sched_test
	./sched_test

clean:
	rm -f sched_test
