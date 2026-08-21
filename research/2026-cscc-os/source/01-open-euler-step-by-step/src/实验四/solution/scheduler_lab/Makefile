CC = gcc
CFLAGS = -Wall -Wextra -g

all: simulator

simulator: simulator.o fcfs.o sjf.o rr.o cfs.o
	$(CC) $(CFLAGS) -o $@ $^

simulator.o: simulator.c scheduler.h
	$(CC) $(CFLAGS) -c simulator.c

fcfs.o: fcfs.c scheduler.h
	$(CC) $(CFLAGS) -c fcfs.c

sjf.o: sjf.c scheduler.h
	$(CC) $(CFLAGS) -c sjf.c

rr.o: rr.c scheduler.h
	$(CC) $(CFLAGS) -c rr.c

cfs.o: cfs.c scheduler.h
	$(CC) $(CFLAGS) -c cfs.c

clean:
	rm -f *.o simulator *_results.csv

run: all
	./simulator