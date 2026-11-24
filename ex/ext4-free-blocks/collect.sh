#!/usr/bin/env bash
# 采集本机 ext4 空闲块管理实况，输出到 evidence/<date>-machine.txt
set -u

OUT_DIR="$(cd "$(dirname "$0")" && pwd)/evidence"
mkdir -p "$OUT_DIR"
OUT="$OUT_DIR/$(date +%F)-machine.txt"

ROOT_SRC="$(findmnt -no SOURCE /)"
DEV_NAME="$(basename "$ROOT_SRC")"
SYSFS="/sys/fs/ext4/$DEV_NAME"

{
  echo "# 采集时间: $(date -Is)  主机: $(hostname)  内核: $(uname -r)"
  echo
  echo "## stat -f /"
  stat -f / 2>/dev/null
  echo
  echo "## findmnt 根分区"
  findmnt -no SOURCE,FSTYPE,OPTIONS /
  echo
  echo "## df -h /"
  df -h /
  echo
  echo "## 所有挂载的块设备文件系统"
  findmnt -t ext4,ext3,ext2,xfs,btrfs,ntfs3,vfat -no TARGET,SOURCE,FSTYPE
  echo
  echo "## $SYSFS 调参项"
  for f in mb_group_prealloc mb_stream_req mb_max_to_scan mb_min_to_scan \
           mb_order2_req mb_prefetch mb_best_avail_max_trim_order \
           delayed_allocation_blocks lifetime_write_kbytes \
           session_write_kbytes reserved_clusters mb_stats; do
    v=$(cat "$SYSFS/$f" 2>/dev/null)
    printf '%-32s = %s\n' "$f" "${v:-<unavailable>}"
  done
  echo
  echo "## 块组规模推算"
  bs=$(stat -f -c %S / 2>/dev/null)
  if [ -n "${bs:-}" ]; then
    echo "block_size            = $bs"
    echo "clusters_per_group    = $((bs * 8))   # 1 bitmap block covers 8*blocksize clusters"
    echo "group_size_bytes      = $((bs * bs * 8))"
    echo "MB_NUM_ORDERS         = $(( $(echo "obase=2;$bs" | bc | tr -d '\n' | wc -c) + 1 ))  # blocksize_bits+2"
  fi
  echo
  echo "## 源码版本标记"
  grep -n "mballoc.c contains the multiblocks" \
       "$(dirname "$0")/kernel-source/mballoc.c" 2>/dev/null
  echo
  echo "## kernel-source md5"
  ( cd "$(dirname "$0")/kernel-source" && md5sum *.c *.h 2>/dev/null | sort )
} > "$OUT"

echo "written: $OUT"
cat "$OUT"
