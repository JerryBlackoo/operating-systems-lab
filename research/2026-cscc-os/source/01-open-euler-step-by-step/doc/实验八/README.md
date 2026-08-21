# 实验八：网络管理

三阶段网络实现：UDP Socket → Netfilter 内核模块 → XDP (eBPF) 高速数据包处理。

## 文档索引

| 文档 | 说明 |
|------|------|
| [实验指导书.md](./实验指导书.md) | 完整实验指导、三阶段实现详解、操作步骤 |
| [设计方案.md](./设计方案.md) | 技术设计思路、三阶段架构、核心代码说明 |
| [测试报告.md](./测试报告.md) | 功能测试、性能对比、技术分析 |

## 快速开始

```bash
# Phase 1: UDP Socket
cd src/实验八/solution/phase1_socket && make
./udp_server &

# Phase 2: Netfilter
cd ../phase2_netfilter && make
sudo insmod netfilter_hook.ko

# Phase 3: XDP
cd ../phase3_xdp && make
clang -O2 -target bpf -c xdp_filter.c -o xdp_filter.o
sudo ip link set dev lo xdp obj xdp_filter.o sec xdp
```

## 文件结构

```
src/实验八/
├── starter/
│   ├── phase1_socket/
│   ├── phase2_netfilter/
│   └── phase3_xdp/
├── solution/         # 参考答案
└── tests/            # 测试脚本
```