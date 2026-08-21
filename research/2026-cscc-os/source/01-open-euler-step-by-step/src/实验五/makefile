# 实验五顶层 Makefile
.PHONY: all starter solution clean test

all: starter solution

starter:
	$(MAKE) -C starter

solution:
	$(MAKE) -C solution

clean:
	$(MAKE) -C starter clean
	$(MAKE) -C solution clean

test:
	sudo ./tests/test_exp5.sh