# solution/Makefile - 完整答案编译配置
CC = gcc
CFLAGS = -Wall -pthread
TARGETS = getpid threadid zombie spinlock producer_consumer send receive write read

all: $(TARGETS)

getpid: getpid_answer.c
	$(CC) -Wall -o getpid $^

threadid: threadid_answer.c
	$(CC) $(CFLAGS) -o threadid $^

zombie: zombie_answer.c
	$(CC) -Wall -o zombie $^

spinlock: spinlock_answer.c
	$(CC) $(CFLAGS) -o spinlock $^

producer_consumer: producer_consumer_answer.c
	$(CC) $(CFLAGS) -o producer_consumer $^

send: send_answer.c
	$(CC) -Wall -o send $^

receive: receive_answer.c
	$(CC) -Wall -o receive $^

write: write_answer.c
	$(CC) -Wall -o write $^

read: read_answer.c
	$(CC) -Wall -o read $^

clean:
	rm -f $(TARGETS)