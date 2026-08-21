obj-m += power_device_cp3.o

# 源码文件叫 power_device_cp3_answer.c，ko 输出叫 power_device_cp3.ko
power_device_cp3-objs := power_device_cp3_answer.o

KDIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)

all:
	make -C $(KDIR) M=$(PWD) modules

clean:
	make -C $(KDIR) M=$(PWD) clean
