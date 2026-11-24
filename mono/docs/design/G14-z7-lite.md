# G14 Z7-Lite 实机启动（文档里程碑）

- 依赖：G1；建议 G6 完成后
- 本文不修改 FSBL/原厂镜像策略，只记录如何用 U-Boot 加载本仓库 `kernel.bin`。

## 已有实测（基线内核）

- `docs/Z7-Lite-SD卡经U-Boot启动内核实录.md`：原厂 `z7_lite_10.img` + U-Boot，
  `fatload mmc 0:1 0x00100000 kernel.bin`、`dcache flush`、`icache flush`、
  `go 0x00100020`，串口输出 `[boot] hello from the kernel` 和 `tick 1..3`。
- `docs/Z7-Lite-JTAG连接与调试实录.md`：OpenOCD 读到 Zynq TAP 和两个 Cortex-A9，
  JTAG 模式下 CPU 停在 BootROM（PC `0xffffff28/0xffffff34`），
  `BOOT_MODE (0xF800025C)=0`；本仓库内核尚未通过 JTAG 下载验证。

## 本次状态

- 当前多 Goal 内核已具备 MMU、进程、FS、shell，但**没有重新在实板上启动过**。
- 因此 G14 按 Goal 允许的“仅文档 + 镜像校验”里程碑处理，不宣称当前内核已实板验证。
- 启动片段见 `docs/Z7-Lite-kernel-boot-snippet.md`；部署脚本为 `tools/deploy-kernel-sd.sh`（默认 dry-run）。
- 证据摘录见 `docs/evidence/z7-boot-20261119.txt`，标注为基线内核日志。

## 验收

```text
[ ] make ARCH=arm bin 生成 kernel.bin
[ ] 复制到 SD FAT 分区并校验 SHA-256
[ ] U-Boot 执行 fatload + cache flush + go 0x00100020
[ ] 串口出现当前内核的 [boot]/[vm]/[fs]/[init]/sh> 日志
```

以上实板项仍待用户签收；文档与脚本已完成。

## 已知限制

- 不集成 OpenOCD 全套下载（JTAG 实录已单独记录）。
- 不删除 DATA 盘原厂镜像。
