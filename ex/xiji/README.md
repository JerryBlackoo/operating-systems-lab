# 希冀（xiji）实验机制 —— QEMU 真实硬件 + 算法双轨实现

本目录把 [`*.md`](.) 笔记里的希冀课内实验落成可运行代码，分两条互相印证的轨道：

| 轨道 | 内容 | 入口 |
|------|------|------|
| **算法轨** | 七个实验的核心数据结构与策略，宿主与内核共用同一份源码 | `src/exp0*.c` |
| **硬件轨** | 用 QEMU 的 CMOS / 8259 / 8254 / CR0.PG+#PF / ATA 真实设备验证同一批机制 | `qemu/hw/` |

## 一键验收

```sh
cd /home/jerry/Projects/from-scratch/operating-systems-lab/ex/xiji
make verify
```

`make verify` 依次做三件事，任一不满足即失败：

1. `make test` —— 宿主 gcc 跑七个实验的算法断言；
2. `make -C qemu run` —— BIOS 引导 → 保护模式 → 硬件自检 → 算法断言，QEMU 以退出码 **33** 成功结束；
3. `verify.py` —— 检查 `build/traces/qemu.stdout` 里的硬件证据行、缺页计数必须恰为 4、目录树确实写入了扇区，并且输出中不得出现任何 `FAIL`。

单独使用：

```sh
make test      # 仅宿主算法
make run       # 仅 QEMU（TCG）
make run QEMU=qemu-system-x86_64 ACCEL=kvm CPU_MODEL=host   # KVM 加速
```

## 硬件轨验证了什么

| 实验 | 真实设备路径 | 通过判据 |
|------|--------------|----------|
| lab1 | CMOS `0x70/0x71`、debugcon `0xe9`、`isa-debug-exit` `0xf4` | RTC 值为合法 BCD；退出码 33 |
| exp01/02 | **8259 PIC** 重映射 + **8254 PIT** IRQ0 | `hlt` 期间 tick 计数增长 ≥2，IRQ0 未被掩码 |
| exp03 | **物理内存**分区（`0x200000` 起） | 三块分区各写独立指纹后互不覆盖；释放中间块不损坏邻居 |
| exp04 | 段相对寻址落到不同物理页 | 两段各写各读，无串扰 |
| exp05 | **CR0.PG + CR3 + #PF** | 4 次按需缺页、4 个物理帧；写保护缺页的 error code 带 P\|W 位 |
| exp06 | **IDE 主通道** `0x1F0–0x1F7` | `IDENTIFY` 返回型号，通道不 BUSY |
| exp07 | **ATA PIO LBA28** 写 `LBA 120` + `FLUSH CACHE` | 目录树序列化后写入并原样读回 |

## 算法轨的要点

- **exp01** 就绪/阻塞队列抽成带 `count` 的 `struct queue`，每步校验 head/tail/计数一致；含空队列、中间摘除、200 步随机 block/wake 压力。
- **exp02** FCFS/SJF/RR 共用 runner，只替换插入与轮转；验证稳定排序、tail 维护、单节点与空队列边界、平均周转时间。
- **exp03** 按基址升序的块数组，切分与**相邻合并**都在 `alloc/free` 里完成；`invariant_ok()` 强制"有序、无空洞、无重叠、总量守恒"。
- **exp04** `apply/diaodu/zhuanhuan` + 首次适配装入 + **FIFO 换出**（笔记里的思考题），并做物理区间反向校验。
- **exp05** FIFO/LRU/OPT 共用框架；额外用一份独立实现交叉核对缺页数，并验证帧数单调性与 OPT 下界。
- **exp06** SDT→DCT→COCT→CHCT 四级表，忙标志自下而上检查，等待队列为 FIFO 且释放时按序唤醒；删除设备区分"忙/仍有等待者/不存在"。
- **exp07** 树形 FS，节点池**可回收复用**，名字长度严格校验，`xiji_exp07_format()` 把树序列化给硬件轨写盘。

## 目录结构

```text
ex/xiji/
  common/            # 宿主/内核共用的输出与断言
  src/               # 七个实验（算法轨）
  qemu/
    hw/              # PIC/PIT/Paging(#PF)/ATA/CMOS + hw_verify
    kernel*.S/.c/.ld # 保护模式入口与 kmain
    load-kernel.S    # stage2：读盘 + 建 GDT + 切保护模式
  host/runner.c      # make test
  verify.py          # 自动化门禁
  *.md               # 希冀笔记原文
```

### 内存布局（QEMU 镜像）

| 地址 | 用途 |
|------|------|
| `0x07C00` | stage1（MBR） |
| `0x07E00` | stage2：INT 13h 读盘、GDT、切保护模式 |
| `0x20000` | 内核镜像（64 KiB 对齐，避开引导区且不跨 INT 13h 边界） |
| `0x40000` | 页目录 |
| `0x41000` | 内核恒等映射页表（0–4 MiB） |
| `0x42000` | 按需映射页表（4–8 MiB） |
| `0x50000` | 按需分配的物理帧池 |
| `0x9F000` | 引导栈顶 |

## 与课内 `*hard.c` 的关系

希冀平台仍要求在官方框架里填空提交；`src/` 给出可直接对号入座的完整逻辑与判据，QEMU 轨则证明这些机制能与真实设备语义对齐。二者不替代 GXemul/MIPS 课环境本身。
