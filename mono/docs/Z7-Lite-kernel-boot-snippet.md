# Z7-Lite：U-Boot 加载本仓库内核

适用：原厂 SD 镜像已能启动 U-Boot，跳线为 **SD 启动**（最上面两根短接），
UART0 115200 8N1。当前多 Goal 内核尚未重新在实板验证；下面是基线内核已验证过的命令。

## 1. 生成裸二进制

```bash
cd /home/jerry/Projects/from-scratch/operating-systems-lab/mono
make ARCH=arm bin
ls -l build/arm/kernel.bin
sha256sum build/arm/kernel.bin
```

## 2. 复制到 SD 卡 FAT 分区

```bash
# 先确认目标卡，避免写错磁盘
tools/deploy-kernel-sd.sh /path/to/SD/FAT
# 确认无误后写入
tools/deploy-kernel-sd.sh --apply /path/to/SD/FAT
```

## 3. U-Boot 命令

```text
fatload mmc 0:1 0x00100000 kernel.bin
dcache flush
icache flush
go 0x00100020
```

说明：

- `0x00100000` 是链接地址和异常向量表所在处，镜像必须加载到这里。
- `0x00100020` 是 `_start` / ELF entry；不要从 `0x00100000` 直接跳（基线实验里会触发 undefined instruction）。
- `fatload` 经过 D-cache，数据可能还是脏行；先 `dcache flush`、`icache flush` 再从入口执行。
- `go 0x00100000` 在刷缓存后是否可用尚未复验，不要写成已解决。

## 4. 期望日志

当前内核（RISC-V QEMU 已验收）应出现：

```text
[boot] operating-systems-lab: minimal kernel
[vm] ...
[fs] formatted: ...
[init] root filesystem ready
sh>
```

ARM 实机若只跑到内核演示而没有 `sh>`，是因为 ARM 用户态尚未启用，不是串口或 FS 故障。

## 5. 回退

重启后不要在 U-Boot 倒计时按键，让它加载原厂 `image.ub` 即可回到 Debian。
