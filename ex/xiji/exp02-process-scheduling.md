# 希冀 实验二：进程调度

> Source: https://blog.csdn.net/m0_74172897/article/details/143329589  
> Path hint: `/headless/Desktop/os/project2/{fcfs,sjf,rr}hard.c`

## Tasks
1. **FCFS** — `fcfs()` append `p` to ready tail
2. **SJF** — insert by ascending run time
3. **RR** — rotate unfinished process to tail after quantum

## FCFS insert
```c
void fcfs() {
    if (!ready) { ready = p; pfend = p; }
    else { pfend->link = p; pfend = p; }
}
```

## Build
```bash
gcc fcfshard.c -o fcfshard && ./fcfshard
# similarly sjfhard.c / rrhard.c
```

## Takeaway
Three policies, one runner; only the insert/rotate rule changes.
