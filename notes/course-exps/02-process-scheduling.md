# 实验二：进程调度

> Source: https://blog.csdn.net/2201_75721523/article/details/142908319  
> Cross-ref: https://blog.csdn.net/m0_74172897/article/details/143329589

## Goal
- Know three-level scheduling: long-term (job), mid-term (swap), short-term (CPU)
- Implement FCFS, SJF, Round-Robin on a PCB ready queue

## PCB sketch
```c
typedef struct pcb {
    char name[10];
    char state;
    int ntime;   // needed CPU time
    int rtime;   // already run
    struct pcb *link;
} PCB;
```

## Algorithms
| Algo | Ready-queue rule | Notes |
|------|------------------|-------|
| FCFS | append to tail | simple; convoy effect |
| SJF  | insert by ascending `ntime` | better avg wait; starvation risk |
| RR   | run head for one quantum, then rotate to tail | fair; quantum tuning matters |

## Implementation tips
- `running()` always advances `ready` head; insert logic must keep that invariant
- FCFS insert: empty → set head/tail; else `tail->link = p; tail = p`
- SJF: walk list, insert before first longer job; handle “already running” head specially if required by template
- RR: on quantum expiry, detach head and append to tail if unfinished

## Takeaway
Scheduler = **ordering policy on the ready queue** + a thin runner that ticks `rtime` until `ntime`.
