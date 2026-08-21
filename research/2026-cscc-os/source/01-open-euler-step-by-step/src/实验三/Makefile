# 实验三顶层 Makefile
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
	./tests/test_exp3.sh