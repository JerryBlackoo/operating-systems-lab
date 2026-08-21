CC = gcc
CFLAGS = -Wall -Wextra -std=c99 -D_DEFAULT_SOURCE
LDFLAGS =

OBJS = disk.o fs.o dir.o main.o
TARGET = fs_test

all: $(TARGET)

$(TARGET): $(OBJS)
	$(CC) $(LDFLAGS) -o $(TARGET) $(OBJS)

disk.o: disk.c disk.h
	$(CC) $(CFLAGS) -c disk.c

fs.o: fs.c fs.h disk.h
	$(CC) $(CFLAGS) -c fs.c

dir.o: dir.c dir.h fs.h disk.h
	$(CC) $(CFLAGS) -c dir.c

main.o: main.c fs.h disk.h dir.h
	$(CC) $(CFLAGS) -c main.c

clean:
	rm -f $(OBJS) $(TARGET) *.img

.PHONY: all clean
