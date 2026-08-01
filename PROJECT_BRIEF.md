# Operating Systems Lab — 项目草案

## 项目方向
**从零写容器运行时 (Go)**

### 为什么做这个
你的 OS 学习路径已经造了完整的教学 OS（M0-M11），但那是"从底层向上"造。容器运行时是"从应用层向下"看——用 namespace + cgroup + overlayfs 隔离进程，是理解 Linux 内核抽象的最短路径。而且这个是真实世界每天都在用的技术。

### 约束
- 不依赖 Docker、containerd、runc 等现有容器运行时
- 只使用 Go 标准库 + `syscall` 包（Go 的 `os/exec` 和 `syscall` 足够）
- 不依赖 `libcontainer` 或 `runc` 库

### 交付物
一个 CLI 工具 `ctr`，支持：
```
ctr run --name hello busybox-rootfs /bin/sh
ctr ps
ctr exec hello /bin/ls
ctr stop hello
ctr rm hello
```

### 核心功能
1. **namespaces**: PID, mount, network, UTS, IPC, user（可选）
2. **cgroups v2**: 内存和 CPU 限制（`--memory 64m --cpus 0.5`）
3. **rootfs**: overlayfs 挂载（lower=busybox-rootfs, upper=container-layer, work=work, merged=merged）
4. **chroot/pivot_root**: 切换根文件系统
5. **网络**: veth pair + bridge（可选，首次可不做）
6. **进程管理**: 跟踪容器 PID，支持 stop/exec/rm

### 验收标准
- `ctr run busybox-rootfs /bin/echo hello` 返回 "hello"
- 容器内 `ps aux` 只看到自己的进程
- `--memory 64m` 限制被内核强制执行
- 连续运行 3 个容器互不干扰

### 结构建议
```
cmd/ctr/main.go        # CLI 入口
pkg/container/         # 容器生命周期
pkg/namespace/         # namespace 管理
pkg/cgroup/            # cgroup 管理
pkg/filesystem/        # overlayfs 挂载
pkg/network/           # 网络（可选）
tests/                 # 集成测试脚本
```

### 参考分支
`reference` 分支将包含参考实现。

### 周期估计
- 基础版 (namespaces + rootfs): 1 周
- cgroup + 进程管理: 1 周
- 网络 + 完整测试: 1 周
