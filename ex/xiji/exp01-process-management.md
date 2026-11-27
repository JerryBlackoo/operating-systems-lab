# 希冀 实验一：进程管理

> Source: https://blog.csdn.net/m0_74172897/article/details/143345968

## Task
Framework already creates/destroys processes; implement **block** and **wake** between ready & blocked queues.

## `block()` sketch
- If blocked queue empty: head = tail = ready head
- Else: append ready head to blocked tail
- Set `state = 'b'`, advance ready pointer, clear new tail link

## `wake()` sketch
- If blocked empty → error message
- Else move blocked head onto ready (append if ready nonempty, else become sole ready)
- Set ready state `'R'`

## Verify
```bash
gcc processmanagehard.c -o processmanagehard
./processmanagehard
```

## Takeaway
Same PCB queue discipline as course-exps lab1; watch empty-queue and double-pointer (head/tail) updates.
