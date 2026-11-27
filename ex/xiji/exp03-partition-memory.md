# 希冀 实验三：分区存储管理

> Source: https://blog.csdn.net/m0_74172897/article/details/143507901

## Implement
`First_fit`, `Best_fit`, `Worst_fit`, and `free_mem` (platform name; not always `free`).

## Policy intent
- **First-fit**: first adequate free block
- **Best-fit**: smallest adequate free block
- **Worst-fit**: largest free block

Common path: scan free list → exact match or split → update metadata.

## Free path
Mark free, **coalesce** adjacent free neighbors, keep ordering expected by the chosen fit strategy.

## Takeaway
Draw the free-list before coding; most bugs are missed merges or wrong “best/worst” comparisons.
