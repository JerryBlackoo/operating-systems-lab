# 希冀 实验五：请求分页式存储管理

> Source: https://blog.csdn.net/m0_74172897/article/details/143754399

## Implement three solvers on a fixed reference string
1. **FIFO_solve** — circular victim pointer among frames
2. **LRU_solve** — replace frame with smallest last-use timestamp; update on hit
3. **OPT_solve** — replace page with farthest next use (or never used again)

Report fault count and hit ratio (lab hard-codes 320 references in the sample printouts).

## Compare
- FIFO: simple, Belady risk
- LRU: practical, needs recency metadata
- OPT: oracle lower bound, not online-realizable without future knowledge

## Takeaway
Same reference string + frame count → different fault curves; use that to internalize locality.
