# M3 服务化 IPC（设计备忘）

## 常驻路径（`MICRO_POST_BOOT_RESIDENT`）

1. `service_boot_prepare()` 为 sid=1..3 安装 endpoint capability。
2. `loader_spawn_user()` 从 initrd 加载 `sm.elf` / `pm.elf` / `init.elf`。
3. `resident_trap` + `user_syscall_backend`：per-thread `ucall_words[]` 保存
   `syscall_call`，避免 `IPC_CALL` 阻塞时全局状态被覆盖。
4. init：`_start` 越权 `ABI_IPC_SEND` → `[cap] denied: missing right SEND`；
   spawn 经 `user_ipc_call(pm_ep)` → PM `ABI_PM_SPAWN`；stub 经 `TAG_READY`
   回执后打印 `[init] ready`。

## 与 `closure.c` 的分工

| 路径 | 用途 |
| --- | --- |
| `closure_run()` | `BOOT_MODULES=syscall` 模块 demo：双 AS IPC + 缺页 |
| resident boot | 默认镜像：M3 服务组合 |

## 已知边界

- SM 尚未处理动态 register/lookup 请求（仅 `[sm] serving` + recv 循环）。
- stub fs/console 只发 ready 后退出，非 M7 shell。
- `ABI_CAP_MINT_TO` 仅为 SM 侧预留桩。