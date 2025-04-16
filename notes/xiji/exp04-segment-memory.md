# 希冀 实验四：段式存储管理

> Source: https://blog.csdn.net/m0_74172897/article/details/143608906

## Functions
1. **`apply()`** — read name, segment count (cap 10), each size; compute relative bases & total
2. **`diaodu()`** — load process if free mem enough; `realaddr = rel + occupied`; bump occupied
3. **`zhuanhuan()`** — `(proc, seg, off) → realaddr + off` with existence / bounds checks

## Thinking question
Add FIFO replacement when mem insufficient: evict earliest loaded process until space fits, then continue `diaodu`.

## Takeaway
Segment lab is bookkeeping: segment table + occupancy counter + translation helper.
