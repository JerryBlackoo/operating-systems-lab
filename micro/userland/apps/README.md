# apps 最小用户程序

一个独立的回显客户端就能描述应用的最小状态。客户端持有预置端点句柄，通过端口发出一条请求，核对结果后结束。

## 最小模型

```text
App = (endpoint, value, state, result)

READY ──call(ECHO, [value, 0, 0, 0])──> WAITING ──正确应答──> DONE
                                          └──错误或应答不符──> FAILED
```

1. `start(endpoint, value)` 以 `ECHO` 为标签，消息字为 `[value, 0, 0, 0]`。
2. 端口 `call` 与运行库的 `user_ipc_call` 同构：同步返回状态，成功时填写应答。
3. `on_reply(status, msg)` 检查状态、`ECHO_REPLY` 标签、`reply == 0` 以及完整的回显字 `[value, 0, 0, 0]`。
4. `finish(result)` 向测试端报告结果，交还本次持有的句柄并结束。

## 不变量

- 一轮只发一个请求，结束时只报告一个结果。
- `DONE` 需要成功状态、正确的应答标签和全部四个回显字。
- 终止路径恰好交还一次端点；重复 `start` 或 `on_reply` 返回 `false`。
- 应用通过端口提交消息，端点以句柄表示；端口由运行库或测试替身提供。 [用户态运行库](../libs/README.md)

## 独立验证

让回显替身依次返回正确应答、错误标签、错误载荷、被污染的消息字和失败状态。检查只有第一种进入 `DONE`，每一轮都只报告一次并交还端点引用。

## QEMU demo

demo 在 U-mode 用真实 `ecall(ABI_IPC_CALL)` 提交请求；测试陷阱处理器扮演回显替身，逐轮返回上述五种结果。`echo.c` 只持有端点、发起调用并核对结果，不含任何服务端逻辑。

```sh
make -C micro/userland/apps run
```

从仓库根目录运行，成功时输出 `[apps] PASS` 并退出 QEMU。

## 自举接线

```sh
make -C micro run BOOT_MODULES='init libs apps'
```

apps 只依赖 ABI 头和 arch 陷阱帧；在自举清单里与 init、libs 并列运行，各自输出 `PASS`。
