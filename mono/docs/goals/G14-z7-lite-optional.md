# [G14] Z7-Lite 实机启动（可选）

**依赖**：G1（建议 G6 完成后）  
**建议 Agent**：A7 或专用硬件 Agent

## 背景

已有实录：

- `docs/Z7-Lite-SD卡经U-Boot启动内核实录.md`
- `docs/Z7-Lite-JTAG连接与调试实录.md`
- `docs/Z7-Lite-从模拟CPU到真实板子-导读.md`

本 Goal **不修改** FSBL/原厂镜像策略；仅验证本仓库 `kernel.bin` 经 U-Boot 加载。

## 教材对照

- 板级手册 + 上述 docs；王道/汤小丹无强制题号。

## 目标

1. `make ARCH=arm bin` 产物可复制到 SD FAT（见 SD 卡实录路径约定）。
2. U-Boot 环境变量或 `boot.cmd` 片段写入 `docs/Z7-Lite-kernel-boot-snippet.md`。
3. 实机串口日志摘录进 `docs/evidence/z7-boot-YYYYMMDD.txt`（可脱敏）。
4. 记录 QEMU vs 实机 tick 间隔差异（不强行改时钟驱动为「假 1s」）。

## 交付

- 文档与（若有）`tools/deploy-kernel-sd.sh` dry-run 说明
- **不要求** 实机上也完成 G13 交互 shell

## 验收

- 实机：上电后 UART 出现 `[boot] hello from the kernel`
- 或：用户签收「仅文档 + 镜像校验」里程碑，在 `docs/roadmap.md` 标记 G14 状态

## 非目标

- _JTAG 调试 OpenOCD 全套集成（除非用户另开 Goal）
- 删除 DATA 盘原厂镜像

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。