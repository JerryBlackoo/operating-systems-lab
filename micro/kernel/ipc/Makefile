CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g -ffreestanding -fno-builtin
CPPFLAGS ?= -I. -I../objects -I../memory -I../sched -I../../shared \
	-I../syscall -DSCHED_HOST_TEST

ipc_test: ipc.c ipc_test.c ipc.h ../sched/sched.c ../sched/sched.h \
	../objects/object.c ../objects/object.h ../memory/frame.c ../memory/as.c \
	../memory/memory.h
	$(CC) $(CPPFLAGS) $(CFLAGS) ipc.c ipc_test.c ../sched/sched.c \
		../objects/object.c ../memory/frame.c ../memory/as.c -o $@

.PHONY: test clean
test: ipc_test
	./ipc_test

clean:
	rm -f ipc_test
