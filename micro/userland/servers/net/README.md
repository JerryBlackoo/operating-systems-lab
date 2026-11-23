# net IPv4 数据报收发

最小网络服务在一个静态 IPv4 地址上提供 UDP 数据报收发。链路替身交付和接收完整 IP 包，服务维护本地端口绑定和接收队列。

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
