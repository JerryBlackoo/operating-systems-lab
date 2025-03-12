# 实验三：存储器 / 分区管理

> Source: https://blog.csdn.net/2201_75721523/article/details/142908491  
> Cross-ref: https://blog.csdn.net/m0_74172897/article/details/143507901

## Goal
- Contrast fixed vs variable partitions
- Implement First-fit / Best-fit / Worst-fit + free (coalesce)

## Fixed partitions
- Pre-split memory; one job per partition
- **Internal fragmentation** common
- Allocation table: base, length, free flag

## Variable partitions
- Carve on demand → no internal holes inside a job’s region
- Creates **external fragmentation**; free must **merge adjacent free blocks**
- Maintain allocated table + free list

## Placement policies
| Policy | Free-list order / scan | Pros | Cons |
|--------|------------------------|------|------|
| First-fit | address-ordered; take first fit | simple; easy coalesce | more scans on avg |
| Best-fit | size-asc; smallest adequate | leaves large holes | many tiny remnants |
| Worst-fit | size-desc; largest hole | keeps leftovers larger | can exhaust big holes early |

## Coding checklist
- `First_fit` / `Best_fit` / `Worst_fit`: find block ≥ request; exact → take; else split
- `free` / `free_mem`: mark free, merge with prev/next neighbors, keep list ordered

## Takeaway
Partition management is a **free-list search + split/merge** problem; policy only changes which hole you pick.
