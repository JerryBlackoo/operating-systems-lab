#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

KERNEL="${KERNEL:-${ROOT_DIR}/kernel-rv}"
QEMU_BIN="${QEMU_BIN:-qemu-system-riscv64}"

check_kernel "${KERNEL}" "kernel-rv"
need_command "${QEMU_BIN}"
qemu_version_note "${QEMU_BIN}"

TEST_IMAGE_PATH="$(resolve_test_image rv)"
OUTPUT_FILE="$(mktemp)"
CLEANUP_PATHS+=("${OUTPUT_FILE}")

qemu_cmd=(
  "${QEMU_BIN}"
  -machine virt
  -kernel "${KERNEL}"
  -m "${MEM}"
  -nographic
  -smp "${SMP}"
  -bios default
  -drive "file=${TEST_IMAGE_PATH},if=none,format=raw,id=x0"
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0
  -no-reboot
  -device virtio-net-device,netdev=net
  -netdev user,id=net
  -rtc base=utc
)

print_qemu_command "${qemu_cmd[@]}"
set +e
run_qemu_streaming "${OUTPUT_FILE}" "${qemu_cmd[@]}"
status=$?
set -e

check_basic_output "${OUTPUT_FILE}" "${status}" "RISC-V"
