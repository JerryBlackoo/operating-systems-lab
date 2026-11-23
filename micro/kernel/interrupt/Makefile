CC      ?= cc
CFLAGS  ?= -std=c11 -O2 -g -Wall -Wextra -Werror

TARGET  := test_irq
SRCS    := irq.c test_irq.c
HDRS    := irq.h

.PHONY: all test clean

all: $(TARGET)

$(TARGET): $(SRCS) $(HDRS)
	$(CC) $(CFLAGS) -o $@ $(SRCS)

test: $(TARGET)
	./$(TARGET)

clean:
	rm -f $(TARGET)
