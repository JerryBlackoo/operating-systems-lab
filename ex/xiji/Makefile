.PHONY: test verify run debug clean

SRC := src

EXP_SRCS := $(SRC)/exp01_process.c $(SRC)/exp02_sched.c $(SRC)/exp03_partition.c \
	$(SRC)/exp04_segment.c $(SRC)/exp05_paging.c $(SRC)/exp06_device.c $(SRC)/exp07_filesys.c

CFLAGS := -Og -g -Wall -Wextra -I.

test: host/xiji-host
	./host/xiji-host

verify:
	python3 verify.py

host/xiji-host: host/runner.c $(EXP_SRCS)
	$(CC) $(CFLAGS) -o $@ host/runner.c $(EXP_SRCS)

run debug:
	$(MAKE) -C qemu $@

clean:
	rm -f host/xiji-host
	$(MAKE) -C qemu clean 2>/dev/null || true
	rm -rf ../../mono/build/xiji-qemu