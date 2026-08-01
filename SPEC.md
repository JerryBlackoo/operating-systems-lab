# Operating Systems Lab — 完整规格

> 本规格定义容器运行时 `ctr` 的全部功能契约。测试即 oracle：你的实现必须通过 `tests/` 中的全部验收测试。
> 参考实现位于 `reference` 分支（闭卷完成后对照用）。

---

## 项目: 从零写容器运行时 `ctr`（Go）

### 1. 目标
从零实现一个 Linux 容器运行时：用 namespace + cgroup v2 + overlayfs 隔离进程。
不依赖 Docker/containerd/runc 及其库（libcontainer 等）。只允许 Go 标准库 + `syscall`/`os/exec`。

### 2. CLI 契约
```
ctr run [--name NAME] [--memory LIMIT] [--cpus CPUS] ROOTFS CMD [ARGS...]
ctr ps
ctr exec NAME CMD [ARGS...]
ctr stop NAME
ctr rm NAME
```

| 子命令 | 参数 | 行为 |
|---|---|---|
| `run` | `--name`（默认自动生成 `ctr-<pid>`）、`--memory`（如 `64m`）、`--cpus`（如 `0.5`）、`ROOTFS`（overlay 下层目录）、`CMD` | 创建并前台运行容器。进程退出后容器自动清理 |
| `ps` | — | 列出存活容器：`NAME PID STATUS`（每行一个），无容器时输出空 |
| `exec` | `NAME`、`CMD` | 在已运行容器内执行命令，输出其 stdout/stderr，透传退出码 |
| `stop` | `NAME` | 向容器主进程发 SIGTERM，2 秒后未退出则 SIGKILL |
| `rm` | `NAME` | 删除已停止容器，清理 overlay 层与 cgroup |

错误：容器不存在 → stderr 输出 `ctr: container <NAME> not found`，退出码 1。
参数错误 → 打印 usage，退出码 2。

### 3. 隔离要求（核心验收点）

| 维度 | 要求 | 验证方式 |
|---|---|---|
| PID namespace | 容器内 `ps` 只能看到自己的进程 | 测试 2 |
| Mount namespace | 容器看不到宿主文件系统（除 rootfs 与必要挂载） | 测试 3 |
| UTS namespace | 容器内 hostname 与宿主不同（`ctr run --name h1 ... hostname` 输出 `h1`） | 测试 2 |
| IPC namespace | 与宿主 IPC 隔离 | 抽查 |
| cgroup v2 内存限制 | `--memory 64m` 必须被内核强制执行 | 测试 4 |
| cgroup v2 CPU 限制 | `--cpus 0.5` 必须写入 cpu.max | 测试 5 |
| overlayfs | 容器写操作不污染 ROOTFS；`rm` 后 upper 层被清理 | 测试 6 |

### 4. 生命周期要求
- `run` 前台阻塞直到主进程退出；Ctrl-C 时转发 SIGTERM 给容器进程
- 主进程退出后：容器状态变 stopped，overlay 挂载点卸载
- `stop` → SIGTERM；超时 2s → SIGKILL
- `rm` 只允许删除 stopped 容器（运行中 → 报错退出码 3）

### 5. 输出格式
- `ctr run --name foo ... /bin/echo hello` → 输出 `hello`，退出码 0
- `ctr ps` → `foo 1234 running`
- `ctr exec foo /bin/echo hi` → `hi`，退出码 0
- 所有日志/错误走 stderr，退出码非 0

### 6. rootfs 约定
- ROOTFS 参数是 overlay lowerdir（宿主机路径）
- 运行时创建 `work/` 与 `upper/`（默认 `./.ctr-<name>/`），容器退出后保留，`rm` 时删除
- 容器内必须挂载：`/proc`（默认选项）、`/dev`（最小 devtmpfs）、`/sys`（只读，可选）

### 7. 验收测试（tests/ 中的测试）

| 测试 | 命令 | 期望 |
|---|---|---|
| t1 | `ctr run --name c1 ROOTFS /bin/echo hello` | 输出 `hello`，退出码 0 |
| t2 | `ctr run --name h1 ROOTFS /bin/sh -c 'hostname; ps'` | 输出 `h1`；`ps` 只显示 ≤3 个进程 |
| t3 | `ctr run --name c2 ROOTFS /bin/ls /host-marker` | 失败（宿主文件不可见） |
| t4 | `ctr run --name mem1 --memory 64m ROOTFS /bin/sh -c '...'` 分配 >64MiB | 进程被 OOM 杀死或分配失败 |
| t5 | 运行 `--cpus 0.5` 容器后检查 cgroup 文件 | `cpu.max` 内容为 `50000 100000` |
| t6 | `run` 写 `/foo` 文件 → `rm` → ROOTFS 中无 `/foo` | overlay 隔离生效 |

---

## 测试运行器

`tests/run_tests.sh`（bash，需 root 或 sudo；在测试专用 cgroup 子树下运行）：
```
sudo tests/run_tests.sh --rootfs ./busybox-rootfs
```
- 每个测试独立打印 `PASS`/`FAIL`
- 全部通过退出码 0
- 失败测试清理残留容器与挂载点

---

## 交付物清单（提交到 main 分支）
1. `cmd/ctr/main.go` — CLI 入口
2. `pkg/container/` — 容器生命周期（run/stop/rm/exec、状态机）
3. `pkg/namespace/` — namespace 创建与切换
4. `pkg/cgroup/` — cgroup v2 资源限制
5. `pkg/filesystem/` — overlayfs 挂载/卸载、rootfs 准备
6. `tests/run_tests.sh` — 验收测试
7. `docs/ARCH.md` — 架构图：进程树、挂载栈、cgroup 层级
8. `docs/test_log.md` — 测试日志
