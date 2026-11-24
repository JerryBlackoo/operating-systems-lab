#!/usr/bin/env bash
# 构建并运行最小两级 x86 bootloader
set -euo pipefail
cd "$(dirname "$0")"
make
make run
