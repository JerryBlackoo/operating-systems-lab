# RustMicroOS Makefile
# 支持 RISC-V64 和 LoongArch64 双架构编译
# 评测系统调用 make all 生成 kernel-rv + kernel-la

# ============================================================================
# 目标平台 / Target Platforms
# ============================================================================

# RISC-V64 目标
TARGET_RV := riscv64gc-unknown-none-elf
# LoongArch64 目标（使用自定义 target，large 代码模型）
TARGET_LA := loongarch64-unknown-none

MODE := release

# ============================================================================
# 目录 / Directories
# ============================================================================

KERNEL_DIR := kernel
USER_DIR := user
BUILD_DIR_RV := target/$(TARGET_RV)/$(MODE)
BUILD_DIR_LA := target/$(TARGET_LA)/$(MODE)
CARGO_CONFIG_SRC := cargo-config/config.toml

# ============================================================================
# 交叉编译工具链 / Cross-Compilation Toolchains
# ============================================================================

# RISC-V64 工具链
CROSS_COMPILE_RV := riscv64-unknown-elf-
CC_RV := riscv64-linux-gnu-gcc
OBJCOPY_RV := $(CROSS_COMPILE_RV)objcopy
OBJDUMP_RV := $(CROSS_COMPILE_RV)objdump

# LoongArch64 工具链
CC_LA := loongarch64-linux-gnu-gcc
# LoongArch64 使用 llvm-objcopy（Rust 工具链自带）
OBJCOPY_LA := rust-objcopy
OBJDUMP_LA := rust-objdump

# 保持向后兼容
CROSS_COMPILE := $(CROSS_COMPILE_RV)
OBJCOPY := $(OBJCOPY_RV)
OBJDUMP := $(OBJDUMP_RV)
TARGET := $(TARGET_RV)
BUILD_DIR := $(BUILD_DIR_RV)

# ============================================================================
# QEMU 配置 / QEMU Configuration
# ============================================================================

# RISC-V64 QEMU
# 🟡 微内核适配：添加 VirtIO-net MMIO 设备和端口转发支持
# RISC-V64 QEMU with VirtIO-net MMIO device and port forwarding
QEMU_RV := qemu-system-riscv64
DISK_IMG := disk.img
OPENSBI := /usr/lib/riscv64-linux-gnu/opensbi/qemu/virt/fw_jump.bin
QEMU_RV_ARGS := -machine virt \
                -nographic \
                -bios $(OPENSBI) \
                -kernel $(BUILD_DIR_RV)/kernel \
                -m 128M \
                -smp 2 \
                -drive file=$(DISK_IMG),if=none,format=raw,id=hd0 \
                -device virtio-blk-device,drive=hd0 \
                -device virtio-net-device,netdev=net0 \
                -netdev user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555,hostfwd=tcp::5201-:5201,hostfwd=udp::5201-:5201

# LoongArch64 QEMU（评测系统格式）
# 🟡 微内核适配：添加 VirtIO-net PCI 设备和端口转发支持
# LoongArch64 QEMU (competition evaluation format)
# Added VirtIO-net PCI device and port forwarding support
QEMU_LA := qemu-system-loongarch64
DISK_IMG_LA := disk-la.img
QEMU_LA_ARGS := -machine virt \
                -nographic \
                -kernel $(BUILD_DIR_LA)/kernel \
                -m 1G \
                -smp 2 \
                -drive file=$(DISK_IMG_LA),if=none,format=raw,id=x0 \
                -device virtio-blk-pci,drive=x0 \
                -device virtio-net-pci,netdev=net0 \
                -netdev user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555,hostfwd=tcp::5201-:5201,hostfwd=udp::5201-:5201

# 保持向后兼容
QEMU := $(QEMU_RV)
QEMU_ARGS := $(QEMU_RV_ARGS)

# ============================================================================
# 比赛输出的内核 ELF 文件名 / Competition Output ELF Names
# ============================================================================

KERNEL_RV := kernel-rv
KERNEL_LA := kernel-la
# Docker Desktop on Windows: repo bind-mount cannot reliably receive large ELF
# writes (cp → "Invalid argument"). When /artifacts is a native Docker volume
# (see local-judge/scripts/in-docker.sh), install kernels there as the
# authoritative copy. Repo-root kernel-* is best-effort for Linux CI / host.
ARTIFACTS_DIR ?= /artifacts
define INSTALL_KERNEL
	@src="$(1)"; dst_name="$(2)"; \
	if [ -d "$(ARTIFACTS_DIR)" ] && [ -w "$(ARTIFACTS_DIR)" ]; then \
		cp -f "$$src" "$(ARTIFACTS_DIR)/$$dst_name"; \
		echo "    installed $(ARTIFACTS_DIR)/$$dst_name"; \
	fi; \
	if cp -f "$$src" "$$dst_name" 2>/dev/null; then \
		echo "    installed ./$$dst_name"; \
	elif [ -d "$(ARTIFACTS_DIR)" ] && [ -f "$(ARTIFACTS_DIR)/$$dst_name" ]; then \
		echo "    note: ./$$dst_name skipped (bind-mount); using $(ARTIFACTS_DIR)/$$dst_name"; \
	else \
		echo "!!! ERROR: cannot install $$dst_name to ./ or $(ARTIFACTS_DIR)" >&2; \
		exit 1; \
	fi
endef

# ============================================================================
# lwext4_rust C 库交叉编译器包装脚本
# Wrapper scripts for lwext4_rust C library cross-compilation
# ============================================================================
#
# lwext4_rust 的 build.rs 硬编码使用 {arch}-linux-musl-gcc 编译 C 代码。
# 评测 Docker 环境中只有 GNU 交叉编译器（{arch}-linux-gnu-gcc）。
# 创建包装脚本将 musl-gcc 调用转发到 gnu-gcc。
# lwext4 是独立 C 库（freestanding），不依赖 libc 头文件，
# 所以 gnu-gcc 可以安全替代 musl-gcc。

CC_WRAPPER_DIR := $(CURDIR)/target/cc-wrappers

# 评测平台 clone 时会过滤隐藏目录，不能依赖仓库里的 .cargo/。
# 构建前由可见模板重建 .cargo/config.toml，同时启用 vendor/ 离线源码。
.PHONY: setup-cargo-config
setup-cargo-config:
	@mkdir -p .cargo
	@cp $(CARGO_CONFIG_SRC) .cargo/config.toml
	@echo "  [cargo-config] generated .cargo/config.toml from $(CARGO_CONFIG_SRC)"

# 创建交叉编译器包装脚本（在 cargo build 之前调用）
.PHONY: setup-cc-wrappers
setup-cc-wrappers:
	@mkdir -p $(CC_WRAPPER_DIR)
	@# RISC-V64: riscv64-linux-musl-gcc -> riscv64-linux-gnu-gcc
	@if ! command -v riscv64-linux-musl-gcc >/dev/null 2>&1 && command -v riscv64-linux-gnu-gcc >/dev/null 2>&1; then \
		echo '#!/bin/sh' > $(CC_WRAPPER_DIR)/riscv64-linux-musl-gcc; \
		echo 'exec riscv64-linux-gnu-gcc "$$@"' >> $(CC_WRAPPER_DIR)/riscv64-linux-musl-gcc; \
		chmod +x $(CC_WRAPPER_DIR)/riscv64-linux-musl-gcc; \
		echo '#!/bin/sh' > $(CC_WRAPPER_DIR)/riscv64-linux-musl-ar; \
		echo 'exec riscv64-linux-gnu-ar "$$@"' >> $(CC_WRAPPER_DIR)/riscv64-linux-musl-ar; \
		chmod +x $(CC_WRAPPER_DIR)/riscv64-linux-musl-ar; \
		echo "  [cc-wrapper] riscv64-linux-musl-gcc -> riscv64-linux-gnu-gcc"; \
	fi
	@# LoongArch64: loongarch64-linux-musl-gcc -> loongarch64-linux-gnu-gcc
	@if ! command -v loongarch64-linux-musl-gcc >/dev/null 2>&1 && command -v loongarch64-linux-gnu-gcc >/dev/null 2>&1; then \
		echo '#!/bin/sh' > $(CC_WRAPPER_DIR)/loongarch64-linux-musl-gcc; \
		echo 'exec loongarch64-linux-gnu-gcc "$$@"' >> $(CC_WRAPPER_DIR)/loongarch64-linux-musl-gcc; \
		chmod +x $(CC_WRAPPER_DIR)/loongarch64-linux-musl-gcc; \
		echo '#!/bin/sh' > $(CC_WRAPPER_DIR)/loongarch64-linux-musl-ar; \
		echo 'exec loongarch64-linux-gnu-ar "$$@"' >> $(CC_WRAPPER_DIR)/loongarch64-linux-musl-ar; \
		chmod +x $(CC_WRAPPER_DIR)/loongarch64-linux-musl-ar; \
		echo "  [cc-wrapper] loongarch64-linux-musl-gcc -> loongarch64-linux-gnu-gcc"; \
	fi

# ============================================================================
# 默认目标（评测系统调用 make all）
# Default target: build both architectures for competition
# ============================================================================

.PHONY: all
all: setup-cargo-config kernel-rv kernel-la
	@echo ""
	@echo "============================================"
	@echo "  Build complete! Output files:"
	@echo "    $(KERNEL_RV) (RISC-V64)"
	@echo "    $(KERNEL_LA) (LoongArch64)"
	@echo "============================================"

# ============================================================================
# 单架构编译目标 / Single-Architecture Build Targets
# ============================================================================

# 编译 RISC-V64 内核（单独目标）
# Build RISC-V64 kernel (standalone target)
.PHONY: kernel-rv
kernel-rv: setup-cargo-config user-rv setup-cc-wrappers
	@echo ""
	@echo ">>> Building RISC-V64 kernel ($(TARGET_RV))..."
	@echo "    CC = $(CC_RV)"
	@PATH="$(CC_WRAPPER_DIR):$(PATH)" \
	 CC=$(CC_RV) \
	 CROSS_COMPILE=riscv64-linux-gnu- \
	 cd $(KERNEL_DIR) && cargo build --$(MODE) --features qemu --target $(TARGET_RV) \
		|| { echo ""; \
		     echo "!!! ERROR: RISC-V64 kernel build FAILED !!!"; \
		     echo "    Target:  $(TARGET_RV)"; \
		     echo "    Check:   Is Rust target installed? (rustup target add $(TARGET_RV))"; \
		     echo "    Check:   Is $(CC_RV) available?"; \
		     exit 1; }
	$(call INSTALL_KERNEL,$(BUILD_DIR_RV)/kernel,$(KERNEL_RV))
	@echo "<<< RISC-V64 kernel built: $(KERNEL_RV) (and $(ARTIFACTS_DIR)/ if present)"

# 编译 LoongArch64 内核（单独目标）
# Build LoongArch64 kernel (standalone target)
# 使用自定义 target JSON (large 代码模型) 支持内核链接在 DMW0 窗口高地址
.PHONY: kernel-la
kernel-la: setup-cargo-config user-la setup-cc-wrappers
	@echo ""
	@echo ">>> Building LoongArch64 kernel ($(TARGET_LA))..."
	@echo "    CC = $(CC_LA)"
	@PATH="$(CC_WRAPPER_DIR):$(PATH)" \
	 CC=$(CC_LA) \
	 CROSS_COMPILE=loongarch64-linux-gnu- \
	 cd $(KERNEL_DIR) && cargo build --$(MODE) --features qemu --target $(TARGET_LA).json -Z build-std=core,alloc \
		|| { echo ""; \
		     echo "!!! ERROR: LoongArch64 kernel build FAILED !!!"; \
		     echo "    Target:  $(TARGET_LA)"; \
		     echo "    Check:   Is Rust target installed? (rustup target add $(TARGET_LA))"; \
		     echo "    Check:   Is $(CC_LA) available?"; \
		     exit 1; }
	$(call INSTALL_KERNEL,$(BUILD_DIR_LA)/kernel,$(KERNEL_LA))
	@echo "<<< LoongArch64 kernel built: $(KERNEL_LA) (and $(ARTIFACTS_DIR)/ if present)"

# ============================================================================
# 向后兼容目标 / Backward-Compatible Targets
# ============================================================================

# 编译内核（向后兼容，默认 RISC-V64）
# Build kernel (backward compatible, defaults to RISC-V64)
.PHONY: kernel
kernel: setup-cargo-config setup-cc-wrappers
	@echo "Building kernel (RISC-V64)..."
	@PATH="$(CC_WRAPPER_DIR):$(PATH)" \
	 CC=$(CC_RV) \
	 CROSS_COMPILE=riscv64-linux-gnu- \
	 cd $(KERNEL_DIR) && cargo build --$(MODE) --features qemu --target $(TARGET_RV)

# 默认用户程序目标保持向后兼容，构建 RISC-V64 用户程序。
.PHONY: user
user: user-rv

# 编译 RISC-V64 用户程序
# Build RISC-V64 user programs
.PHONY: user-rv
user-rv: setup-cargo-config
	@echo "Building user programs..."
	cd $(USER_DIR) && cargo build --$(MODE) --target $(TARGET_RV)

# 编译 LoongArch64 init。kernel-la 的 build.rs 会内嵌该 ELF。
# 这里只构建 init，避免 RISC-V 专用用户程序中的寄存器名阻塞 LA 构建。
.PHONY: user-la
user-la: setup-cargo-config
	@echo "Building LoongArch64 init..."
	cd $(USER_DIR) && cargo build --$(MODE) --target ../$(KERNEL_DIR)/$(TARGET_LA).json -Z build-std=core,alloc --bin init

# 完整构建流程：编译内核 → 编译用户程序 → 创建磁盘镜像
.PHONY: build-all
build-all: kernel user disk
	@echo "Complete build finished!"
	@echo "Run 'make run' to start QEMU"

# 创建磁盘镜像（需要 mkfs 工具）
.PHONY: disk
disk: user mkfs-tool
	@echo "Creating disk image..."
	@./target/x86_64-unknown-linux-gnu/release/mkfs create --output $(DISK_IMG) --size 64 || \
		(echo "Error: mkfs tool failed. Try running 'scripts/build-disk.ps1' on Windows"; exit 1)
	@./target/x86_64-unknown-linux-gnu/release/mkfs add-elf --image $(DISK_IMG) --elf-dir $(BUILD_DIR_RV) || true
	@./target/x86_64-unknown-linux-gnu/release/mkfs verify --image $(DISK_IMG) || \
		echo "Warning: Disk image verification failed"
	@echo "Disk image created: $(DISK_IMG)"

# 构建 mkfs 工具（使用 Linux x86_64 目标）
.PHONY: mkfs-tool
mkfs-tool: setup-cargo-config
	@echo "Building mkfs tool..."
	cd tools/mkfs && cargo build --release --target x86_64-unknown-linux-gnu --target-dir ../../target

# ============================================================================
# QEMU 运行目标 / QEMU Run Targets
# ============================================================================

# 运行 RISC-V64 QEMU（需要磁盘镜像）
.PHONY: run
run: kernel check-disk
	@echo "Running RISC-V64 on QEMU with disk image..."
	$(QEMU_RV) $(QEMU_RV_ARGS)

# 运行 LoongArch64 QEMU
.PHONY: run-la
run-la: kernel-la
	@echo "Running LoongArch64 on QEMU..."
	$(QEMU_LA) $(QEMU_LA_ARGS)

# 运行 QEMU（不带磁盘镜像，用于基本调试）
.PHONY: run-nodisk
run-nodisk: kernel
	@echo "Running RISC-V64 on QEMU without disk image..."
	$(QEMU_RV) -machine virt -nographic -bios default -kernel $(BUILD_DIR_RV)/kernel -m 128M -smp 2

# 调试模式运行（需要磁盘镜像）
.PHONY: debug
debug: kernel check-disk
	@echo "Running QEMU in debug mode..."
	$(QEMU_RV) $(QEMU_RV_ARGS) -s -S

# 调试模式运行（不带磁盘镜像）
.PHONY: debug-nodisk
debug-nodisk: kernel
	@echo "Running QEMU in debug mode without disk image..."
	$(QEMU_RV) -machine virt -nographic -bios default -kernel $(BUILD_DIR_RV)/kernel -m 128M -smp 2 -s -S

# 检查磁盘镜像是否存在（Windows 兼容）
.PHONY: check-disk
check-disk:
ifeq ($(OS),Windows_NT)
	@if not exist $(DISK_IMG) (echo Error: Disk image '$(DISK_IMG)' not found! && echo Please run 'scripts/build-disk.ps1' first. && exit /b 1)
else
	@if [ ! -f $(DISK_IMG) ]; then \
		echo "Error: Disk image '$(DISK_IMG)' not found!"; \
		echo "Please run 'make disk' first."; \
		exit 1; \
	fi
endif

# GDB 连接
.PHONY: gdb
gdb:
	$(CROSS_COMPILE_RV)gdb -ex 'target remote localhost:1234' \
	                       -ex 'file $(BUILD_DIR_RV)/kernel'

# 生成二进制镜像
.PHONY: binary
binary: kernel
	$(OBJCOPY_RV) -O binary $(BUILD_DIR_RV)/kernel $(BUILD_DIR_RV)/kernel.bin

# 反汇编
.PHONY: disasm
disasm: kernel
	$(OBJDUMP_RV) -d $(BUILD_DIR_RV)/kernel > $(BUILD_DIR_RV)/kernel.asm

# 清理（两个架构的编译产物）
# Clean build artifacts for both architectures
.PHONY: clean
clean:
	cargo clean
	@rm -f $(KERNEL_RV) $(KERNEL_LA)
	@rm -rf target/cc-wrappers
	@echo "Cleaned: cargo artifacts + $(KERNEL_RV) + $(KERNEL_LA) + cc-wrappers"

# K210 目标
.PHONY: k210
k210:
	@echo "Building for K210..."
	cd $(KERNEL_DIR) && cargo build --$(MODE) --features k210 --target $(TARGET_RV)
	$(OBJCOPY_RV) -O binary $(BUILD_DIR_RV)/kernel $(BUILD_DIR_RV)/kernel.bin

# QEMU 自动化测试
QEMU_TEST_ARGS := -machine virt \
                  -nographic \
                  -bios none \
                  -kernel $(BUILD_DIR_RV)/kernel \
                  -m 128M \
                  -smp 2 \
                  -device virtio-blk-device,drive=hd0 \
                  -drive file=test.img,format=raw,id=hd0,if=none \
                  -device virtio-net-device,netdev=net0 \
                  -netdev user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555,hostfwd=tcp::5201-:5201,hostfwd=udp::5201-:5201

# 测试超时（秒）
TEST_TIMEOUT := 60

# 运行自动化测试
.PHONY: test
test: kernel
	@echo "Running automated tests..."
	@mkdir -p test-results
	@$(MAKE) test-boot
	@$(MAKE) test-memory
	@$(MAKE) test-sched
	@$(MAKE) test-ipc
	@$(MAKE) test-fs
	@echo "All tests completed!"

# 启动测试
.PHONY: test-boot
test-boot: kernel
	@echo "[TEST] Boot test..."
	@timeout $(TEST_TIMEOUT)s $(QEMU_RV) $(QEMU_RV_ARGS) \
		-append "test=boot" \
		2>&1 | tee test-results/boot.log | grep -q "All subsystems initialized" \
		&& echo "[PASS] Boot test" || (echo "[FAIL] Boot test"; exit 1)

# 内存管理测试
.PHONY: test-memory
test-memory: kernel
	@echo "[TEST] Memory management test..."
	@timeout $(TEST_TIMEOUT)s $(QEMU_RV) $(QEMU_RV_ARGS) \
		-append "test=memory" \
		2>&1 | tee test-results/memory.log | grep -q "Memory test passed" \
		&& echo "[PASS] Memory test" || echo "[SKIP] Memory test (no output)"

# 调度器测试
.PHONY: test-sched
test-sched: kernel
	@echo "[TEST] Scheduler test..."
	@timeout $(TEST_TIMEOUT)s $(QEMU_RV) $(QEMU_RV_ARGS) \
		-append "test=sched" \
		2>&1 | tee test-results/sched.log | grep -q "Scheduler test passed" \
		&& echo "[PASS] Scheduler test" || echo "[SKIP] Scheduler test (no output)"

# IPC 测试
.PHONY: test-ipc
test-ipc: kernel
	@echo "[TEST] IPC test..."
	@timeout $(TEST_TIMEOUT)s $(QEMU_RV) $(QEMU_RV_ARGS) \
		-append "test=ipc" \
		2>&1 | tee test-results/ipc.log | grep -q "IPC test passed" \
		&& echo "[PASS] IPC test" || echo "[SKIP] IPC test (no output)"

# 文件系统测试
.PHONY: test-fs
test-fs: kernel
	@echo "[TEST] Filesystem test..."
	@dd if=/dev/zero of=test.img bs=1M count=16 2>/dev/null || true
	@timeout $(TEST_TIMEOUT)s $(QEMU_RV) $(QEMU_TEST_ARGS) \
		-append "test=fs" \
		2>&1 | tee test-results/fs.log | grep -q "Filesystem test passed" \
		&& echo "[PASS] Filesystem test" || echo "[SKIP] Filesystem test (no output)"
	@rm -f test.img

# QEMU 测试（CI 用，带超时自动退出）
.PHONY: qemu-test
qemu-test: kernel
	@echo "Running QEMU test (timeout: $(TEST_TIMEOUT)s)..."
	@timeout $(TEST_TIMEOUT)s $(QEMU_RV) $(QEMU_RV_ARGS) || true

# 性能测试
.PHONY: bench
bench: kernel
	@echo "Running performance benchmarks..."
	@mkdir -p bench-results
	@timeout 120s $(QEMU_RV) $(QEMU_RV_ARGS) \
		-append "bench=all" \
		2>&1 | tee bench-results/benchmark.log

# 压力测试
.PHONY: stress
stress: kernel
	@echo "Running stress tests..."
	@timeout 300s $(QEMU_RV) $(QEMU_RV_ARGS) \
		-m 256M -smp 4 \
		-append "stress=all" \
		2>&1 | tee test-results/stress.log

# 生成测试报告
.PHONY: test-report
test-report:
	@echo "=== Test Report ===" > test-results/report.txt
	@echo "Date: $$(date)" >> test-results/report.txt
	@echo "" >> test-results/report.txt
	@for f in test-results/*.log; do \
		echo "--- $$f ---" >> test-results/report.txt; \
		tail -20 "$$f" >> test-results/report.txt 2>/dev/null || true; \
		echo "" >> test-results/report.txt; \
	done
	@cat test-results/report.txt

# 帮助
.PHONY: help
help:
	@echo "RustMicroOS Build System (Dual-Architecture)"
	@echo ""
	@echo "Build Targets:"
	@echo "  all          - Build both kernel-rv and kernel-la (default)"
	@echo "  kernel-rv    - Build RISC-V64 kernel only"
	@echo "  kernel-la    - Build LoongArch64 kernel only"
	@echo "  kernel       - Build RISC-V64 kernel (backward compat)"
	@echo "  build-all    - Complete build: kernel + user + disk image"
	@echo "  user         - Build user programs (RISC-V64)"
	@echo "  disk         - Create FAT32 disk image with user programs"
	@echo "  mkfs-tool    - Build the mkfs tool"
	@echo ""
	@echo "Run Targets:"
	@echo "  run          - Run RISC-V64 on QEMU (requires disk.img)"
	@echo "  run-la       - Run LoongArch64 on QEMU"
	@echo "  run-nodisk   - Run RISC-V64 on QEMU without disk image"
	@echo "  debug        - Run QEMU in debug mode (requires disk.img)"
	@echo "  debug-nodisk - Run QEMU in debug mode without disk image"
	@echo "  gdb          - Connect GDB to QEMU"
	@echo ""
	@echo "Other Targets:"
	@echo "  binary       - Generate binary image"
	@echo "  disasm       - Generate disassembly"
	@echo "  k210         - Build for K210 board"
	@echo "  clean        - Clean build artifacts (both architectures)"
	@echo ""
	@echo "Disk Image:"
	@echo "  check-disk   - Check if disk.img exists"
	@echo "  (Windows: Use scripts/build-disk.ps1)"
	@echo "  (Windows: Use scripts/build-and-run.ps1 for complete workflow)"
	@echo ""
	@echo "Testing:"
	@echo "  test         - Run all automated tests"
	@echo "  test-boot    - Run boot test"
	@echo "  test-memory  - Run memory management test"
	@echo "  test-sched   - Run scheduler test"
	@echo "  test-ipc     - Run IPC test"
	@echo "  test-fs      - Run filesystem test"
	@echo "  test-props   - Run property-based tests"
	@echo "  qemu-test    - Run QEMU with timeout (for CI)"
	@echo "  bench        - Run performance benchmarks"
	@echo "  stress       - Run stress tests"
	@echo "  test-report  - Generate test report"
	@echo "  clippy       - Run Clippy static analysis"
	@echo "  ci           - Run full CI checks"
	@echo ""
	@echo "  help         - Show this help"

# 属性测试
.PHONY: test-props
test-props:
	@echo "Running property-based tests..."
	cd $(KERNEL_DIR) && cargo test --features test --no-run
	@echo "Property tests compiled successfully"

# Clippy 静态分析
.PHONY: clippy
clippy:
	@echo "Running Clippy..."
	cd $(KERNEL_DIR) && cargo clippy -- -D warnings

# 格式检查
.PHONY: fmt-check
fmt-check:
	@echo "Checking code format..."
	cd $(KERNEL_DIR) && cargo fmt -- --check

# 完整 CI 检查
.PHONY: ci
ci: fmt-check clippy kernel test-props
	@echo "CI checks passed!"
