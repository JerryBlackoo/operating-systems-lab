# net IPv4 数据报收发

最小网络服务在一个静态 IPv4 地址上提供 UDP 数据报收发。链路替身交付和接收完整 IP 包，服务维护本地端口绑定和接收队列。

> **定位**：本模块是**链路替身**，不属于 M0–M7 主线。它的 `[net] PASS` 只证明
> 「端口绑定 + 接收队列 + 校验」这套语义，**不**证明真实网卡收发包。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct net` | `udp.h` | 服务状态：`local_ip`、`mtu`、`ports[]`、绑定的 `net_port` |
| `struct net_port` | `udp.h` | **唯一的链路缝**：`deliver`（把完整 IP 包交给链路）/ `poll`（取下一个入站包） |
| `struct net_address` | `udp.h` | `ip` + `port` |
| `struct net_datagram` | `udp.h` | 一个数据报：`source`（`net_address`）、`payload`、`len` |
| `enum net_result` | `udp.h` | `NET_OK` / `NET_BAD_ARG` / `NET_EXISTS` / `NET_NO_SPACE` / `NET_NOT_FOUND` / `NET_DENIED` / `NET_TOO_LARGE` / `NET_EMPTY` / `NET_BAD_PACKET` / `NET_LINK_ERROR` |

## 与其他模块的关联

| 对方模块 | 关系 |
| --- | --- |
| 无（独立） | 不依赖任何内核模块；`net_port` 全由 demo 的内存替身实现 |
| 与 `block`/`nvme` 的关系 | 结构上同构：都是「策略 + 端口」两层，端口后面可以是替身也可以是真驱动 |
| 内核 | 无。它既不进内核镜像，也不在 initrd 的默认 9 个成员里 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 端口表 + 绑定 | `bind(port, owner)` / `unbind`，拒绝重复绑定与越界端口 | 先把「谁是某个端口的接收者」这件事件清楚 |
| 2. 接收 FIFO | 每个端口一条有界队列，满时返回 `NET_NO_SPACE` | 数据报可能成串到达；队列必须有界才不耗尽内存 |
| 3. MTU 校验 | `send` 时检查 `payload + 头 ≤ mtu` | 超长包在链路层会被丢弃，应在服务层就拒绝并回明确错误 |
| 4. 端口端口化 | `net_port` 的 `deliver`/`poll` | 让链路可以是内存替身（现在）或真网卡（将来） |
| 5. 校验和与解析（未做） | — | 当前把链路交付的包当作「已校验的完整 IP 包」 |

## 最小模型

```text
Net       = (local_ip, mtu, ports)
ports     : UDPPort ⇀ (owner, recv_fifo)
Datagram  = (source_ip, source_port, payload)

未绑定 ──bind──> 已绑定 ──unbind──> 未绑定
                    ├──rx──> 队列接收一个完整数据报
                    └──recv──> 取出队首数据报
```

| 操作 | 契约 |
| --- | --- |
| `bind(caller, port)` | 为一个空闲端口建立绑定和有界队列 |
| `send(caller, port, dst, bytes)` | 校验发送者与长度，生成一个单播 IPv4/UDP 包交给链路替身 |
| `rx(packet)` | 校验报头、长度、校验和与目的地址，将数据报交给目标端口。非法包、未绑定端口和队列满均丢弃此包 |
| `recv(caller, port)` | 取出一条数据报及源地址，空队列返回 `EMPTY` |
| `unbind(caller, port)` | 由端口所有者撤销绑定并释放队列 |

本模型接受无选项、未分片的 IPv4 单播包。无选项 IPv4 首部为 20 字节，UDP 首部为 8 字节，发送数据须落在链路 MTU 和协议长度字段允许的范围内。接收时校验 IPv4 首部和非零的 UDP 校验和。 [RFC 791](https://www.rfc-editor.org/rfc/rfc791.html) · [RFC 768](https://www.rfc-editor.org/rfc/rfc768.html)

## 不变量

1. 每个端口最多一个所有者，只有该所有者能使用绑定发送、接收或解绑。
2. 接收队列按到达顺序交付完整数据报，成功接收消耗一项。
3. 发送成功表示链路替身已接受该包。数据报在交付前保持原有字节顺序和长度。

## 独立验证

向绑定端口注入两条合法报文、一条长度错误的报文和一条发往其他地址的报文。检查合法报文按序完整返回，其余报文被丢弃。再检查重复绑定和超 MTU 发送。

## QEMU demo

`udp.c` 编解码 IPv4/UDP 字节报文、核对校验和，并管理本地端口。链路替身捕获发送报文并注入接收队列。

```sh
make -C micro/userland/servers/net run
```

从仓库根目录运行，成功时输出 `[net] PASS` 并退出 QEMU。

## 本轮接线状态（勿过声称）

本轮**没有**接通真实网卡或链路层。demo 使用链路替身捕获发送报文并注入接收
队列，`[net] PASS` 只证明 IPv4/UDP 报头、校验和、端口队列和丢弃规则；
不代表 MMIO 网卡、DMA、IRQ 或 U-mode net 服务已经工作。剩余差距：链路层
驱动、DMA/IRQ 接线、分片/选项、ARP/ICMP 和真实网络往返。
