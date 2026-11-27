# 希冀 lab1：内核、Boot 与 printf

> Source: https://blog.csdn.net/m0_74172897/article/details/143845361

## Setup
```bash
cd <studentId>-lab
git pull
git checkout lab1
# fix CROSS_COMPILE in include.mk to /OSLAB/compiler/usr/bin/mips_4KC-
make   # produces gxemul/vmlinux
```

## Concepts
1. **Bootstrapping**: ROM/FLASH stage1 → RAM stage2 bootloader → kernel
2. GXemul simplifies boot: loads ELF kernel and jumps to entry
3. **Makefile / ELF**: sections, linker script, load addresses
4. **MIPS memory**: kernel in kseg0 (cached unmapped) before MMU is up
5. ABI: args in `a0–a3`, retval in `v0/v1`, `ra` return address

## Exercises (summary)
- **1.1** Point `CROSS_COMPILE` correctly and build `vmlinux`
- **1.2** Finish simplified `readelf` to print section headers (`"%d:0x%x\n"`)
- Later exercises: early boot asm, `printf` / console output path—complete **all** Exercise items required by the platform

## Takeaway
lab1 connects toolchain → ELF image → emulator boot → first kernel print; debugging starts with correct cross prefix and load address.
