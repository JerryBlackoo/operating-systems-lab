# starter/Makefile - 学生填空版编译配置
CC = gcc
CFLAGS = -Wall -pthread
TARGETS = getpid threadid zombie spinlock producer_consumer send receive write read

all: $(TARGETS)

getpid: getpid.c
	$(CC) -Wall -o $@ $^

threadid: threadid.c
	$(CC) $(CFLAGS) -o $@ $^

zombie: zombie.c
	$(CC) -Wall -o $@ $^

spinlock: spinlock.c
	$(CC) $(CFLAGS) -o $@ $^

producer_consumer: producer_consumer.c
	$(CC) $(CFLAGS) -o $@ $^

send: send.c
	$(CC) -Wall -o $@ $^

receive: receive.c
	$(CC) -Wall -o $@ $^

write: write.c
	$(CC) -Wall -o $@ $^

read: read.c
	$(CC) -Wall -o $@ $^

clean:
	rm -f $(TARGETS)