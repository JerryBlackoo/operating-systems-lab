# 实验四：段式存储管理

> Source: https://blog.csdn.net/2201_75721523/article/details/142908912  
> Cross-ref: https://blog.csdn.net/m0_74172897/article/details/143608906

## Goal
- 2D virtual address: `(segment #, offset)`
- Segment table maps logical segments → physical base
- Dynamic load / address translation / (optional) FIFO replace

## Concepts
- Segments are **variable length**, logically complete units (code/data/stack…)
- Segments of one process need **not** be contiguous in physical memory
- Translation: `PA = segment.base + offset` after bounds check
- Missing segment → fault; may need allocation or replacement

## Typical lab functions (希冀)
1. `apply()` — record process name, #segments, each length; compute relative bases
2. `diaodu()` — if free mem ≥ total size, assign physical bases (`rel + occupied`), mark loaded
3. `zhuanhuan()` — given process, seg#, offset → physical address (validate ranges)

## Extension
FIFO segment replacement when memory insufficient: evict earliest loaded process’s segments until space fits.

## Takeaway
Segmentation trades contiguous pages for **semantic units** and sharing/protection at segment granularity; address math is base+offset with length checks.
