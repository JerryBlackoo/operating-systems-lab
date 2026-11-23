# apps 最小用户程序

一个独立的回显客户端就能描述应用的最小状态。客户端持有预置端点句柄，通过运行库发出一条请求，核对结果后结束。

## 最小模型

```text
App = (endpoint, value, state, result)

READY ──call(ECHO, value)──> WAITING ──正确应答──> DONE
                                └──错误或应答不符──> FAILED
```

1. `start(endpoint, value)` 以 `ECHO` 为标签，消息字为 `[value, 0, 0, 0]`。
2. `on_reply(status, msg)` 检查状态、`ECHO_REPLY` 标签和回显值。
3. `finish(result)` 向测试端报告结果，交还本次持有的句柄并结束。

## 不变量

- 一轮只发一个请求，结束时只报告一个结果。
- `DONE` 需要成功状态和正确的应答标签及载荷。
- 应用通过运行库提交消息，端点以句柄表示。 [用户态运行库](../libs/README.md)

## 独立验证

让回显替身依次返回正确应答、错误标签、错误载荷和失败状态。检查只有第一种进入 `DONE`，每一轮都交还端点引用。

## QEMU demo

`echo.c` 实现应用的启动和应答处理。demo 在 U-mode 用回显替身运行四轮，核对报告与端点引用。

```sh
make -C micro/userland/apps run
```

从仓库根目录运行，成功时输出 `[apps] PASS` 并退出 QEMU。
