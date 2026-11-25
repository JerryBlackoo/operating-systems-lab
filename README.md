# operating-systems-lab

同一仓库内并列两种内核实验树：

| 目录 | 类型 | 状态 |
|------|------|------|
| [`mono/`](mono/) | **Monolithic kernel**（RISC-V + ARM Zynq） | 已实现 G0–G14 教学子集 |
| [`micro/`](micro/) | **Microkernel**（RISC-V） | M0–M7 主线：`make demo` / 常驻 `run` + initrd U-mode init + SM/PM/fs/console/shell + virtio→ext4→VFS；20 模块 `BOOT_MODULES=all` 仍独立 PASS（见 [`micro/docs/goals/`](micro/docs/goals/README.md)） |

## 构建与运行（宏内核）

在仓库根目录（转发到 `mono/`）：

```sh
make && make run
make ARCH=arm && make ARCH=arm run
```

或直接进入子目录：

```sh
cd mono && make run
```

文档、Goal、验收命令均在 [`mono/README.md`](mono/README.md) 与 [`mono/docs/`](mono/docs/)。

## 构建与运行（微内核）

```sh
cd micro
make clean all
timeout 8s make run
```

启动模型见 [`micro/kernel/boot/README.md`](micro/kernel/boot/README.md)，RISC-V arch 层见 [`micro/kernel/arch/riscv/README.md`](micro/kernel/arch/riscv/README.md)。

微内核的目标、基线与依赖顺序见 [`micro/docs/goals/README.md`](micro/docs/goals/README.md)（M0–M7）与 [`micro/docs/goals/GLOBAL-CONTRACT.md`](micro/docs/goals/GLOBAL-CONTRACT.md)。

独立的微内核骨架副本（若仍在维护）：[`/home/jerry/Projects/micro/operating-systems-lab`](/home/jerry/Projects/micro/operating-systems-lab)；本仓库 `micro/` 为同源实现入口。