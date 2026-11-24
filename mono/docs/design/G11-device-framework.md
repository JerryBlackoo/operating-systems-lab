# G11 设备独立层与驱动框架

- 王道：5.1 I/O 管理概述；5.2 设备独立性软件
- 汤小丹：第 9 章 I/O 子系统

## 实现

- `struct devsw { name, read, write }`；`devices[]` 注册 console、null、zero。
- `/dev/null`：写丢弃、读 EOF；`/dev/zero`：读返回零、写丢弃。
- `sys_open` 识别 `/dev/null`、`/dev/zero`，把设备号记录在 `struct file`。
- `sys_read/sys_write` 对设备文件走 devsw 语义，对普通文件走 G10 FS。
- 控制台仍由 `syscall.c` 直接调用 `console_*`（fd 0/1/2），避免用户页拷贝和字符设备缓冲耦合。

## 与教材的对应

- devsw 等价于设备开关表；major/minor 简化为数组下标。
- 设备独立性体现在“同一套 read/write fd 接口”，而不是完整 VFS。
- 不做假脱机、设备分配表、USB、DMA 通用框架、可中断键盘。

## 路径说明

- 架构 UART 驱动仍在 `kernel/arch/{riscv,arm}/uart.c`；没有强行搬到 `kernel/drivers/uart.c`，因为两端寄存器与初始化差异大，搬移只会增加空壳。`devsw` 的 console 项保留，实际字符 I/O 由 syscall 层直接调用。

## 验收

```bash
make && timeout 10s make run
# [dev] null/zero ok (devices=3)
```

## 已知限制

- 没有真正把 UART 注册成可 open 的字符设备；`/dev/console` 未实现。
