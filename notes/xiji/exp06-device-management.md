# 希冀 实验六：设备管理

> Source: https://blog.csdn.net/m0_74172897/article/details/144415341

## Model
Hierarchical I/O control tables: **SDT → DCT → COCT → CHCT**, plus PCB wait queues.

## `DeleteDevice()`
- Reject missing / busy devices
- Delete associated PCB waiters as required
- Possibly delete controller if unused
- Unlink device node from SDT list and `free`

## `ApplyDevice()`
Allocation walks busy flags bottom-up:
1. Device busy → wait on device queue
2. Else controller busy → wait on controller
3. Else channel busy → wait on channel
4. Else success: mark device/controller/channel busy, attach process to run list

## Takeaway
Device allocation is a **multi-level busy check + wait-queue** problem mirroring real channel/controller architecture.
