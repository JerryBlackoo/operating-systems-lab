# 实验一：进程管理

> Source notes: https://blog.csdn.net/2201_75721523/article/details/142908239  
> Cross-ref (希冀): https://blog.csdn.net/m0_74172897/article/details/143345968

## Goal
- Understand process vs program; PCB fields and role
- Know process control primitives: create / destroy / block / wake
- Simulate ready ↔ running ↔ blocked transitions

## Key ideas
1. **Process** = program + data + PCB; dynamic, concurrent, independent, asynchronous
2. **PCB** is the OS’s handle for a process (id, state, CPU context, resources, priority, queue link)
3. Three classic states: ready → running (dispatch); running → ready (time slice); running → blocked (wait); blocked → ready (event)

## Simulation focus
Implement / understand:
- Create & destroy on a PCB linked list
- `block()`: move ready-queue head into blocked queue (`state='b'`)
- `wake()`: move blocked-queue head back to ready (`state='R'`)

Default simplification: always operate on **queue head**. Extending to named PIDs needs search + splice.

## Takeaway
Process control is mostly **queue + state machine** work on PCB nodes; correctness hinges on empty-queue edge cases and pointer updates.
