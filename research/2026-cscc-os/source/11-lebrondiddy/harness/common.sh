#!/usr/bin/env bash

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"

TEST_GROUP="${TEST_GROUP:-basic}"
EXPECTED_GROUPS="${EXPECTED_GROUPS:-}"
TIMEOUT_SECONDS="${TIMEOUT_SECONDS:-60}"
STALL_TIMEOUT_SECONDS="${STALL_TIMEOUT_SECONDS:-120}"
MEM="${MEM:-1G}"
SMP="${SMP:-1}"
KEEP_TEST_IMAGE="${KEEP_TEST_IMAGE:-1}"
TEST_IMAGE_LAYOUT="${TEST_IMAGE_LAYOUT:-nested}"
SKIP_TEST_IMAGE_VALIDATE="${SKIP_TEST_IMAGE_VALIDATE:-0}"

CLEANUP_PATHS=()

cleanup_harness() {
  if ((${#CLEANUP_PATHS[@]})); then
    rm -rf "${CLEANUP_PATHS[@]}"
  fi
}
trap cleanup_harness EXIT

fail() {
  local code="$1"
  shift
  echo "FAIL: $*" >&2
  exit "${code}"
}

need_command() {
  local command_name="$1"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    fail 3 "${command_name} was not found. Run this harness inside zhouzhouyi/os-contest:20260510."
  fi
}

check_kernel() {
  local kernel="$1"
  local artifact="$2"
  if [[ ! -f "${kernel}" ]]; then
    fail 2 "${kernel} does not exist. Run make all first; do not create placeholder ${artifact} files."
  fi
}

qemu_version_note() {
  local qemu_bin="$1"
  local version
  version="$("${qemu_bin}" --version | sed -n '1p')"
  echo "INFO: ${version}" >&2
  if [[ "${version}" != *"9.2.1"* ]]; then
    echo "WARN: contest README records QEMU 9.2.1; this executable differs." >&2
  fi
}

default_testcase_dir() {
  local arch="$1"
  case "${arch}" in
    rv) echo "${ROOT_DIR}/.m0-baselines/ByteOS/tools/testcase-riscv64" ;;
    la) echo "${ROOT_DIR}/.m0-baselines/ByteOS/tools/testcase-loongarch64" ;;
    *) fail 2 "unknown harness architecture ${arch}" ;;
  esac
}

build_basic_test_image() {
  local arch="$1"
  local image="${2:-}"
  local testcase_dir="${TESTCASE_DIR:-$(default_testcase_dir "${arch}")}"
  local script="${ROOT_DIR}/contest_capture/raw/github/scripts/basic/basic_testcode.sh"

  [[ -d "${testcase_dir}" ]] || fail 2 "no TEST_IMAGE was provided and ${testcase_dir} is missing."
  [[ -f "${testcase_dir}/busybox" ]] || fail 2 "${testcase_dir}/busybox is missing."
  [[ -d "${testcase_dir}/basic" ]] || fail 2 "${testcase_dir}/basic is missing."
  [[ -f "${script}" ]] || fail 2 "${script} is missing."

  need_command mkfs.ext4

  local staging image_mb
  staging="$(mktemp -d)"
  if [[ -z "${image}" ]]; then
    image="$(mktemp "${TMPDIR:-/tmp}/oskernel-${arch}-basic.XXXXXX.img")"
  fi
  image_mb="${TEST_IMAGE_MB:-96}"
  CLEANUP_PATHS+=("${staging}")
  if [[ "${KEEP_TEST_IMAGE}" != "1" ]]; then
    CLEANUP_PATHS+=("${image}")
  fi

  mkdir -p "${staging}/rootfs"
  case "${TEST_IMAGE_LAYOUT}" in
    flat)
      copy_basic_payload "${testcase_dir}" "${staging}/rootfs" "${script}" "basic"
      ;;
    nested)
      copy_basic_payload "${testcase_dir}" "${staging}/rootfs/glibc" "${script}" "basic"
      copy_basic_payload "${testcase_dir}" "${staging}/rootfs/musl" "${script}" "basic"
      ;;
    *)
      fail 2 "unsupported TEST_IMAGE_LAYOUT=${TEST_IMAGE_LAYOUT}; expected flat or nested."
      ;;
  esac

  rm -f "${image}"
  dd if=/dev/zero of="${image}" bs=1M count="${image_mb}" >/dev/null 2>&1
  mkfs.ext4 -q -b 4096 -F -O ^metadata_csum_seed -d "${staging}/rootfs" "${image}"
  echo "${image}"
}

copy_basic_payload() {
  local testcase_dir="$1"
  local destination="$2"
  local script="$3"
  local marker_group="$4"

  mkdir -p "${destination}"
  cp -a "${testcase_dir}/busybox" "${destination}/"
  cp -a "${testcase_dir}/basic" "${destination}/"
  if [[ -f "${testcase_dir}/libc.so" ]]; then
    cp -a "${testcase_dir}/libc.so" "${destination}/"
  fi
  if [[ -d "${testcase_dir}/bin" ]]; then
    cp -a "${testcase_dir}/bin" "${destination}/"
  fi

  cp "${script}" "${destination}/basic_testcode.sh"
  chmod +x "${destination}/basic_testcode.sh"
  sed -i '/^cd basic$/d' "${destination}/basic/run-all.sh"
  sed -i \
    -e "s/#### OS COMP TEST GROUP START basic ####/#### OS COMP TEST GROUP START ${marker_group} ####/" \
    -e "s/#### OS COMP TEST GROUP END basic ####/#### OS COMP TEST GROUP END ${marker_group} ####/" \
    "${destination}/basic_testcode.sh"
  find "${destination}" -name '._*' -delete
}

oj_image_path() {
  local arch="$1"
  case "${arch}" in
    rv) echo "${ROOT_DIR}/sdcard-rv.img" ;;
    la) echo "${ROOT_DIR}/sdcard-la.img" ;;
    *) fail 2 "unknown harness architecture ${arch}" ;;
  esac
}

resolve_test_image() {
  local arch="$1"
  if [[ -n "${TEST_IMAGE:-}" ]]; then
    [[ -f "${TEST_IMAGE}" ]] || fail 2 "TEST_IMAGE=${TEST_IMAGE} does not exist."
    if [[ "${SKIP_TEST_IMAGE_VALIDATE}" != "1" ]]; then
      validate_test_image_layout "${TEST_IMAGE}"
    fi
    echo "${TEST_IMAGE}"
    return
  fi

  if [[ "${TEST_GROUP}" != "basic" ]]; then
    fail 2 "automatic image creation only supports TEST_GROUP=basic; set TEST_IMAGE for ${TEST_GROUP}."
  fi

  local testcase_dir image
  testcase_dir="${TESTCASE_DIR:-$(default_testcase_dir "${arch}")}"
  image="$(oj_image_path "${arch}")"
  if [[ -d "${testcase_dir}" ]]; then
    TESTCASE_DIR="${testcase_dir}" build_basic_test_image "${arch}" "${image}"
    return
  fi

  if [[ -f "${image}" ]]; then
    validate_test_image_layout "${image}"
    echo "${image}"
    return
  fi

  fail 2 "no TEST_IMAGE was provided, ${testcase_dir} is missing, and ${image} does not exist."
}

validate_test_image_layout() {
  local image="$1"
  need_command debugfs

  case "${TEST_IMAGE_LAYOUT}" in
    flat)
      debugfs -R 'stat /basic_testcode.sh' "${image}" >/dev/null 2>&1 \
        || fail 2 "${image} does not contain /basic_testcode.sh for TEST_IMAGE_LAYOUT=flat."
      ;;
    nested)
      debugfs -R 'stat /glibc/basic_testcode.sh' "${image}" >/dev/null 2>&1 \
        || fail 2 "${image} does not contain /glibc/basic_testcode.sh for TEST_IMAGE_LAYOUT=nested."
      debugfs -R 'stat /musl/basic_testcode.sh' "${image}" >/dev/null 2>&1 \
        || fail 2 "${image} does not contain /musl/basic_testcode.sh for TEST_IMAGE_LAYOUT=nested."
      ;;
    *)
      fail 2 "unsupported TEST_IMAGE_LAYOUT=${TEST_IMAGE_LAYOUT}; expected flat or nested."
      ;;
  esac
}

print_qemu_command() {
  printf 'INFO: QEMU command:' >&2
  printf ' %q' "$@" >&2
  printf '\n' >&2
}

run_qemu_streaming() {
  local output_file="$1"
  shift
  local fifo monitor_pid qemu_pid tee_pid rc status
  fifo="$(mktemp -u "${TMPDIR:-/tmp}/oskernel-qemu-output.XXXXXX.fifo")"
  CLEANUP_PATHS+=("${fifo}")
  mkfifo "${fifo}"

  tee "${output_file}" <"${fifo}" &
  tee_pid=$!

  set +e
  setsid timeout "${TIMEOUT_SECONDS}" "$@" >"${fifo}" 2>&1 &
  qemu_pid=$!
  set -e

  (
    local last_size=-1 last_change now size
    last_change="$(date +%s)"
    while kill -0 "${qemu_pid}" >/dev/null 2>&1; do
      sleep 1
      now="$(date +%s)"
      size="$(wc -c <"${output_file}" 2>/dev/null || echo 0)"
      if [[ "${size}" != "${last_size}" ]]; then
        last_size="${size}"
        last_change="${now}"
      elif ((now - last_change > STALL_TIMEOUT_SECONDS)); then
        echo "HARNESS STALL: no console output for ${STALL_TIMEOUT_SECONDS}s" >&2
        kill -TERM "-${qemu_pid}" >/dev/null 2>&1 || true
        sleep 1
        kill -KILL "-${qemu_pid}" >/dev/null 2>&1 || true
        exit 7
      fi
    done
  ) &
  monitor_pid=$!

  set +e
  wait "${qemu_pid}"
  status=$?
  wait "${monitor_pid}"
  rc=$?
  wait "${tee_pid}"
  set -e

  if [[ "${rc}" -eq 7 ]]; then
    return 7
  fi
  return "${status}"
}

check_basic_output() {
  local output_file="$1"
  local qemu_status="$2"
  local arch_name="$3"
  local normalized_output
  normalized_output="$(mktemp)"
  CLEANUP_PATHS+=("${normalized_output}")
  tr -d '\r' <"${output_file}" >"${normalized_output}"

  if [[ "${qemu_status}" -eq 124 ]]; then
    fail 6 "${arch_name} QEMU timed out after ${TIMEOUT_SECONDS}s."
  fi
  if [[ "${qemu_status}" -eq 7 ]]; then
    fail 7 "${arch_name} console output stalled for more than ${STALL_TIMEOUT_SECONDS}s."
  fi
  if [[ "${qemu_status}" -ne 0 ]]; then
    fail 6 "${arch_name} QEMU exited with status ${qemu_status}."
  fi

  if grep -Eiq 'panic|panicked|page fault|page error|kernel page error|unknown command|could not be run|No [^ ]*_testcode|cannot open shared object file|error while loading shared libraries|execve error' "${normalized_output}"; then
    fail 5 "${arch_name} output contains a failure signal."
  fi

  local expected_groups="${EXPECTED_GROUPS:-}"
  if [[ -z "${expected_groups}" ]]; then
    expected_groups="$(
      sed -n 's/.*#### OS COMP TEST GROUP START \(basic\(-glibc\|-musl\)\?\) ####.*/\1/p' "${normalized_output}" \
        | awk '!seen[$0]++' \
        | paste -sd, -
    )"
  fi
  expected_groups="${expected_groups:-${TEST_GROUP}}"

  local group
  for group in ${expected_groups//,/ }; do
    grep -Fq "#### OS COMP TEST GROUP START ${group} ####" "${normalized_output}" \
      || fail 4 "${arch_name} output is missing the ${group} START marker."
    grep -Fq "#### OS COMP TEST GROUP END ${group} ####" "${normalized_output}" \
      || fail 4 "${arch_name} output is missing the ${group} END marker."
  done

  require_group_order "${normalized_output}" "${arch_name}" "${expected_groups}"
  require_basic_case_evidence "${normalized_output}" "${arch_name}" "${expected_groups}"
  require_multigroup_evidence "${normalized_output}" "${arch_name}" "${expected_groups}"

  echo "PASS: ${arch_name} ${expected_groups} markers and real basic case output found"
}

require_group_order() {
  local output_file="$1"
  local arch_name="$2"
  local expected_groups="$3"
  local previous_line=0
  local group line

  for group in ${expected_groups//,/ }; do
    line="$(
      grep -n -F "#### OS COMP TEST GROUP START ${group} ####" "${output_file}" \
        | head -1 \
        | cut -d: -f1
    )"
    if [[ -z "${line}" ]]; then
      fail 4 "${arch_name} output is missing the ${group} START marker."
    fi
    if ((line < previous_line)); then
      fail 4 "${arch_name} output ran ${group} out of EXPECTED_GROUPS order."
    fi
    previous_line="${line}"
  done
}

require_basic_case_evidence() {
  local output_file="$1"
  local arch_name="$2"
  local expected_groups="$3"

  local missing=0
  local test_case
  local cases=(
    brk chdir clone close dup2 dup execve exit fork fstat getcwd getdents
    getpid getppid gettimeofday mkdir_ mmap mount munmap openat open pipe
    read times umount uname unlink wait waitpid write yield
  )

  for group in ${expected_groups//,/ }; do
    [[ "${group}" == basic* ]] || continue
    if [[ "${group}" == basic* ]] && group_has_line "${output_file}" "${group}" 'Assert Fatal|FAIL|[[:space:]]fail(ed)?|test_.*fail'; then
      fail 5 "${arch_name} ${group} contains a basic failure signal."
    fi
    for test_case in "${cases[@]}"; do
      if ! awk -v group="${group}" -v test_case="${test_case}" '
        function finish_case() {
          if (in_case && seen_start && seen_end) found = 1
          in_case = 0
          seen_start = 0
          seen_end = 0
        }
        $0 == "#### OS COMP TEST GROUP START " group " ####" { in_group = 1; next }
        $0 == "#### OS COMP TEST GROUP END " group " ####" {
          finish_case()
          in_group = 0
          next
        }
        in_group && $0 == "Testing " test_case " :" {
          in_case = 1
          seen_start = 0
          seen_end = 0
          next
        }
        in_group && in_case && $0 ~ /^Testing .+ :$/ {
          finish_case()
          next
        }
        in_group && in_case && /^========== START test_/ {
          seen_start = 1
        }
        in_group && in_case && /^========== END( |$)/ {
          seen_end = 1
        }
        END { exit found ? 0 : 1 }
      ' "${output_file}"; then
        echo "FAIL: missing complete START/END output after ${group} Testing ${test_case} :" >&2
        missing=1
      fi
    done
    if ! awk -v group="${group}" '
      function finish_case() {
        if (in_case && !(seen_start && seen_end)) {
          missing = case_name
        }
        in_case = 0
        seen_start = 0
        seen_end = 0
      }
      $0 == "#### OS COMP TEST GROUP START " group " ####" { in_group = 1; next }
      $0 == "#### OS COMP TEST GROUP END " group " ####" {
        finish_case()
        in_group = 0
        next
      }
      in_group && $0 ~ /^Testing .+ :$/ {
        finish_case()
        case_name = $0
        in_case = 1
        seen_start = 0
        seen_end = 0
        next
      }
      in_group && in_case && /^========== START test_/ {
        seen_start = 1
      }
      in_group && in_case && /^========== END( |$)/ {
        seen_end = 1
      }
      END {
        finish_case()
        if (missing != "") {
          print "FAIL: missing complete START/END output after " group " " missing > "/dev/stderr"
          exit 1
        }
      }
    ' "${output_file}"; then
      missing=1
    fi
  done

  if [[ "${missing}" -ne 0 ]]; then
    fail 5 "${arch_name} basic group printed prologues without complete real case output."
  fi
}

require_multigroup_evidence() {
  local output_file="$1"
  local arch_name="$2"
  local expected_groups="$3"
  local group base

  for group in ${expected_groups//,/ }; do
    base="${group%-glibc}"
    base="${base%-musl}"
    case "${base}" in
      basic)
        ;;
      busybox)
        if ! group_has_line "${output_file}" "${group}" '^testcase busybox .+ (success|fail)$'; then
          fail 5 "${arch_name} ${group} did not emit busybox testcase result lines."
        fi
        ;;
      lua)
        if ! group_has_line "${output_file}" "${group}" '^testcase lua .+ (success|fail)$'; then
          fail 5 "${arch_name} ${group} did not emit lua testcase result lines."
        fi
        ;;
    esac
  done
}

group_has_line() {
  local output_file="$1"
  local group="$2"
  local pattern="$3"
  awk -v group="${group}" -v pattern="${pattern}" '
    $0 == "#### OS COMP TEST GROUP START " group " ####" { in_group = 1; next }
    $0 == "#### OS COMP TEST GROUP END " group " ####" { in_group = 0; next }
    in_group && $0 ~ pattern { found = 1 }
    END { exit found ? 0 : 1 }
  ' "${output_file}"
}
