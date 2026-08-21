#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

KERNEL="${KERNEL:-${ROOT_DIR}/kernel-la}"
QEMU_BIN="${QEMU_BIN:-qemu-system-loongarch64}"

check_kernel "${KERNEL}" "kernel-la"
need_command "${QEMU_BIN}"
qemu_version_note "${QEMU_BIN}"

TEST_IMAGE_PATH="$(resolve_test_image la)"
OUTPUT_FILE="$(mktemp)"
CLEANUP_PATHS+=("${OUTPUT_FILE}")

qemu_cmd=(
  "${QEMU_BIN}"
  -kernel "${KERNEL}"
  -m "${MEM}"
  -nographic
  -smp "${SMP}"
  -drive "file=${TEST_IMAGE_PATH},if=none,format=raw,id=x0"
  -device virtio-blk-pci,drive=x0
  -no-reboot
  -device virtio-net-pci,netdev=net0
  -netdev user,id=net0
  -rtc base=utc
)

print_qemu_command "${qemu_cmd[@]}"
set +e
run_qemu_streaming "${OUTPUT_FILE}" "${qemu_cmd[@]}"
status=$?
set -e

check_basic_output "${OUTPUT_FILE}" "${status}" "LoongArch"
