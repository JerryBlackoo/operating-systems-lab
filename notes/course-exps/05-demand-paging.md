# 实验五：请求分页式存储管理

> Source: https://blog.csdn.net/2201_75721523/article/details/142909110  
> Cross-ref: https://blog.csdn.net/m0_74172897/article/details/143754399

## Goal
- Page / frame / page-table basics
- Demand paging + page-fault path
- Compare FIFO / LRU / OPT replacement

## Basics
- Logical address → page # + offset; page table → frame #
- Page size power-of-two; too small ⇒ long tables; too large ⇒ internal waste
- Demand paging adds: present bit, access/dirty bits, disk address

## Page fault
Generated **during** instruction execution when target page not present; may fault multiple times per instruction; HW must restart the faulting instruction.

## Replacement (lab sims)
| Algo | Victim rule | Lab metric |
|------|-------------|------------|
| FIFO | oldest loaded frame | fault count + hit ratio |
| LRU  | least recently used (timestamp / stack) | usually fewer faults |
| OPT  | farthest next use (oracle) | theoretical lower bound |

Belady’s anomaly: FIFO faults may **increase** when frames increase.

## Takeaway
Virtual memory ≈ page table + fault handler + replacement policy; policy choice dominates fault rate under a fixed reference string.
