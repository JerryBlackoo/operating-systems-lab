# M2：initrd 与独立用户 ELF

## 实现接缝

- `userland/init/user/`：独立 `_start` 与 `user.ld`；`PHDRS` 将 RX text/rodata 与 RW data/bss 分离，不再靠 `--no-rosegment` 规避同页问题。
- `userland/user-common.mk`：用户构建自行发现 `ld.lld/rust-lld` 并在用户输出目录建立链接；`make -C micro user BUILD=<全新目录>` 不依赖内核构建产物。
- `tools/mkinitrd.py`：MIRD v1，member 的 `name+data` 整体按 4 字节对齐。
- `kernel/loader/loader.c`：校验 ELF64/LE/ET_EXEC/EM_RISCV、phdr 形状、文件范围、溢出、W^X 和可执行 entry；按页合并多段权限，支持非对齐段、跨页复制、共享页与 bss 清零。
- 加载失败解除本次段映射并归还引用；`loader_spawn_user` 的失败路径还释放 AS 与 launch 槽。
- initrd 由 BootInfo 保留区保护，loader 直接消费保留的物理页，不再假设多次 `frame_alloc` 能得到连续镜像。
- `kernel/boot/fdt_initrd.c`：`/chosen` 的 `linux,initrd-*`（u32/u64）进入 BootInfo。
- 当前不传 `argv/envp`；只由 `loader_boot_args` 填 a0..a5。无动态链接、PIE 或重定位。

## 验收（2026-11-24）

```bash
make -C micro/kernel/loader test
make -C micro user BUILD=$PWD/micro/build/user-standalone
llvm-readelf -h -l micro/build/user-standalone/init.elf
make -C micro initrd
timeout 5s qemu-system-riscv64 <真实 initrd/drive/kernel 参数>
```

正向日志含 `[init] ready`、`[init] hello from initrd` 与 idle；QEMU 负向将 init ELF 的 `e_machine` 改为 62（EM_X86_64），host 另覆盖 `e_machine=0`。两者均被拒绝；QEMU 日志含 `[loader] reject: bad machine`，随后仍进入 idle 并由 timeout 结束（EXIT=124）。内核符号表不含 `init_main`。

证据：`docs/evidence/M2-2026-11-24.log`；早期单段原型保留在 `M2-2026-11-24-run.log`。