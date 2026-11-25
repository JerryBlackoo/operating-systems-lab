# M7 设计：init → shell → 关机

**状态**：M7 主路径已通过（`make demo` / `tools/demo.sh`）；M5/M6 判别可绿。混合启动：内核 spawn `vm/heaptest/heaptest2`，init `plan=3` 经 PM 拉 fs/console/shell。

## 1. 启动链（2026-11-25 权威）

```text
OpenSBI
  → kernel_init: frame pool / kernel AS / UART+PLIC / storage bringup
  → service_boot_prepare + loader_spawn_user:
       sm, pm, vm, heaptest, heaptest2, init（各自独立 AS）
  → init (plan=3)，**分两阶段 spawn**：
       阶段 1: PM spawn fs, console → 等这两个 TAG_READY
       阶段 2: PM spawn shell       → 等它的 TAG_READY
  → shell: micro> ；stdin 经 console 服务
  → demo: help / cat /motd / ps / echo / poweroff
```

### 为什么必须分阶段（2026-11-25）

shell 一启动就立刻打印 `micro> ` 并开始响应命令。而 fs 服务的只读自检
（挂载 ext4 + 读 /motd 与 /largefile 算 SHA-256）需要时间，且它的日志按
24 B 分块经 console 输出。两者并行时，fs 的输出会插进 shell 的
`commands: help cat ps echo poweroff` 中间，把这行拆成两段：

```text
commands: help cat ps ec[ext4] read /largefile: 204800 bytes
ho poweroff
```

后果不只是观感：`make check` 的 demo 与 `tools/shell_stress.py` 都做整行
匹配，会**间歇性失败**（实测约 1/8 次）。让 shell 等到阶段 1 的 ready
全部到齐再 spawn，是唯一的顺序保证点。

`ready_mask` 按 **image 号**记位（receipt = 0x2000 + image），不是按 spawn
序号——阶段 0 的 fs/console 是 image 1/2，对应 bit1/bit2。

## 2. shell 的独立性与边界

- `userland/shell/user/shell.elf` 是独立 AS，由 PM 通过 `ABI_PM_SPAWN` 创建。
- 启动 cap：ready send（slot 1）与 SM send（slot 3）；FS/PM/console 的
  call 句柄由 shell 自己经 SM lookup 获得，不是启动 cap。
- 输入：shell 向 console 服务发 `TAG_CONSOLE_READ`；console 服务再用
  `ABI_CONSOLE_READ`（**仅 SERVICE_SID_CONSOLE**）从内核有界行队列取行。
  空队列返回 ABI_BUSY，shell 用 ABI_YIELD 轮询。
- 输出：shell 向 console 服务发 `TAG_CONSOLE_WRITE`；console 服务用
  `ABI_CONSOLE_WRITE`（**仅 SERVICE_SID_CONSOLE**）→ console_puts（MMIO 优先）；
  panic 走 SBI。
- cat：shell → FS endpoint（`TAG_FS_PATH` 16 B/块、`TAG_FS_READ` 24 B/块）
  → FS 服务内部：`ABI_BLOCK_READ` 读原始块 → ext4/VFS → 24 B IPC 分块返回；
  not found 返回 `ABI_NOT_FOUND` / `TAG_FS_ERROR`。无共享页。
- ps：shell → PM endpoint（`TAG_PM_PS`）→ PM 自己维护的进程表
  （init/sm/pm 固定登记为 1/2/3；fs/console/shell 用 spawn 返回值登记）。
- poweroff：`ABI_POWEROFF`（仅 shell SID）→ 内核打印
  `[kmain] shutdown requested by shell` → `sbi_shutdown()`（SRST）。

## 3. 关键修复

### 3.1 U-mode AS 的设备 MMIO

`as_share_kernel(as, kernel_root, 2)` 只共享 `0x80000000` 以上的内核 RAM。
UART/PLIC/virtio 在 `0x0c/0x10` 顶层索引 0，未映射。内核代 U-mode syscall
运行时仍使用该 U-mode satp，访问 virtio 通知即缺页；缺页打印又访问 UART
MMIO，形成静默挂死。

修复：`loader_spawn_user()` 在共享内核 RAM 后，用
`as_map_kernel_range()` 把 UART0、PLIC、virtio-mmio 恒等映射到每个用户 AS。
叶子不带 U 位，U-mode 不可访问，只有内核 syscall 路径可用。

### 3.2 virtio 完成判定

完成只由 `used->idx` 推进触发；仅 status 变化不清 busy，避免下一次 submit
与设备旧请求重叠。修复前表现为偶发 `[block] completion timeout`。

### 3.3 PM 进程表

不使用内核 ps 字符串替身。PM 持有 `struct pm_proc { pid, name[12], state, image }`，
按 index 逐条返回；shell 只持有 PM endpoint cap。pid 是 PM 本地合成序号，
**不等同** `ABI_PID_SELF` 返回的 thread object id。

## 4. 已知边界

- M6：`vm` / `heaptest` / `heaptest2` 由内核 `loader_spawn_user` 拉起（非 init/PM）；无 `vm_reserve` / 并发缺页队列（`m6_discriminate` 靠 heaptest2 长 yield）。
- shell `cat` 路径 ≤255 字节，按 `SVC_PATH_CHUNK = 16` 字节/块传入；
  文件内容按 `SVC_DATA_BYTES = 24` 字节/块经 IPC 返回（无共享页）。
- shell 输入经 console 服务 IPC（`TAG_CONSOLE_READ`）；`ABI_CONSOLE_READ`
  只对 console SID 开放，不经 shell 直接调用。
- M5：`tools/m5_discriminate.sh` 已含 block 队列满 `BLOCK_BUSY` + 200KiB largefile 哈希。