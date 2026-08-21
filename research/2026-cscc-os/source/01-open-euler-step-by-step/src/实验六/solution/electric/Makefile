obj-m := power_sensor.o

KDIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)

all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules EXTRA_CFLAGS="-Wno-error"

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean