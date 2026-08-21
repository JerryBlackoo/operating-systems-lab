CLANG = clang
MULTIARCH := $(shell gcc -print-multiarch 2>/dev/null || true)
CFLAGS = -O2 -target bpf -D__BPF_TRACING__ -I/usr/include $(if $(MULTIARCH),-I/usr/include/$(MULTIARCH))

all: xdp_filter.o

xdp_filter.o: xdp_filter.c
	$(CLANG) $(CFLAGS) -c xdp_filter.c -o xdp_filter.o

clean:
	rm -f xdp_filter.o

load:
	sudo ip link set dev lo xdp obj xdp_filter.o sec xdp

unload:
	sudo ip link set dev lo xdp off

status:
	ip link show