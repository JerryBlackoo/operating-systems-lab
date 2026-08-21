# M0 Baseline Test Results

Date: 2026-06-08
Executor: Codex
Ground truth image: `zhouzhouyi/os-contest:20260510`
Image digest on WSL: `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`

## Environment

- Remote host: `ssh -p 2222 lenovo@100.92.143.96`
- Remote workspace: `/home/lenovo/oskernel2025-x`
- Candidate cache: `/home/lenovo/oskernel2025-x/.m0-baselines`
- Logs: `/home/lenovo/oskernel2025-x/logs/m0-baseline`
- Docker engine in WSL works. The official image was pulled on macOS as
  `linux/amd64`, then transferred to WSL via `docker save | ssh docker load`
  because WSL had unstable Docker Hub/proxy access.

Mac cleanup was done after transfer:

```bash
docker rmi zhouzhouyi/os-contest:20260510
```

## Candidate Commits

| Candidate | Commit | Result |
|---|---:|---|
| `oscomp/starry-next` | `85170046dd6beca4125969a2a43c9f7c2ae6e747` | Build blocked by inaccessible git dependency |
| `Starry-OS/StarryOS` | `2e075accf4fb0aefdd1d252ebd9ccf29727d9923` | Build blocked by toolchain bootstrap in official image |
| `Byte-OS/ByteOS` | `06171525001d73f8660c9fb23b11690b32cefe3d` | LOCKED: RV and LA build and boot to shell in official image after dependency prefetch |

## Findings

### `oscomp/starry-next`

Preparation:

- Added `.arceos` dependency at commit `54d5739ce5fc5c7a83f7dd2c119e76fe7567ec63`.
- Pre-downloaded `sdcard-riscv64.img` and `sdcard-loongarch64.img` from
  `Azure-stars/testsuits-for-oskernel` on macOS and copied them to WSL because
  WSL GitHub release download was extremely slow.
- Installed `axconfig-gen` inside the container from `/tmp` to avoid triggering
  the repository's unrelated `nightly-2025-05-20` bootstrap during the tool
  install.

Command attempted:

```bash
docker run --rm \
  -v /home/lenovo/oskernel2025-x/.m0-baselines:/work \
  -v /home/lenovo/oskernel2025-x/logs/m0-baseline:/logs \
  -w /work/starry-next \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'cd /tmp && cargo install axconfig-gen >/tmp/axconfig-install.log 2>&1; cd /work/starry-next; env RUSTUP_TOOLCHAIN=nightly-2025-01-18 make oscomp_run ARCH=riscv64 LOG=info'
```

Result:

- Reached real Cargo build.
- Failed fetching `https://github.com/Starry-OS/axprocess.git#f6f0d08f202be9310ec2b72d73915b5017fac631`.
- With Cargo's libgit2 path, GitHub returned HTTP 401.
- With `CARGO_NET_GIT_FETCH_WITH_CLI=true`, `git fetch` failed with:
  `fatal: could not read Username for 'https://github.com': No such device or address`.
- Local macOS `git ls-remote https://github.com/Starry-OS/axprocess.git` also
  fails with authentication required.

Conclusion:

`starry-next` is not a clean fork baseline at this commit unless the missing
`axprocess` dependency is replaced, vendored, or otherwise recovered. This is a
hard M0 blocker, not just WSL network noise.

### `Starry-OS/StarryOS`

Preparation:

- Pre-downloaded `rootfs-riscv64.img` and `rootfs-loongarch64.img` from
  `Starry-OS/rootfs` on macOS and copied them to WSL.

Command attempted:

```bash
docker run --rm \
  -v /home/lenovo/oskernel2025-x/.m0-baselines:/work \
  -v /home/lenovo/oskernel2025-x/logs/m0-baseline:/logs \
  -w /work/StarryOS \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'make ARCH=riscv64 rootfs build'
```

Result:

- Stalled during `cargo axplat --version` in `make/deps.mk`.
- Cause: repository requires `nightly-2026-02-25` with components not present in
  the official 20260510 image; rustup bootstrap made no progress in the WSL
  network environment during the timebox.

Conclusion:

`StarryOS` may still be the right active upstream, but it is not an immediate
official-image baseline without pre-seeding its 2026-02-25 Rust toolchain and
build helpers.

### `Byte-OS/ByteOS`

Preparation:

- Cloned `Byte-OS/tools` from macOS and copied it into the candidate cache.
- Added a temporary host `yq` binary from `mikefarah/yq v4.44.6` for container
  runs. The official image does not provide `yq`.

Commands attempted:

```bash
make PLATFORM=riscv64-qemu run
env RUSTUP_TOOLCHAIN=nightly-2025-01-18 make PLATFORM=riscv64-qemu build
```

Result:

- Plain run failed early because `yq` is missing.
- ByteOS `rust-toolchain.toml` requests `nightly-2025-02-01` plus
  `rust-analyzer`; the latter download stalled in WSL.
- Forcing `nightly-2025-01-18` and adding `yq` reached Cargo build.
- Build then failed fetching public git dependency
  `https://github.com/Byte-OS/timestamp.git#0394ccfa14db1912047a0a3e7e1c38b027856165`
  due to WSL GitHub timeout.
- macOS verified that the `timestamp` commit exists, so this is a network/cache
  issue rather than a dead dependency.

Conclusion:

ByteOS is not open-box runnable in the official image, but its blocker appears
more recoverable than `starry-next`: add `yq`, remove or bypass `rust-analyzer`
bootstrap, and vendor/preseed git dependencies.

Follow-up M0 execution:

- Claude prefetched the ByteOS git dependencies on macOS and copied
  `byteos-deps.tar.gz` to WSL.
- Codex added the missing `lwext4_rust` submodule dependency:
  `gkostka/lwext4.git#58bcf89a121b72d4fb66334f1693d3b30e4cb9c5`.
- Cargo git dependencies were redirected to local file repositories with
  `CARGO_NET_GIT_FETCH_WITH_CLI=true` and `git config url.*.insteadOf`.
- A temporary container-only `sudo` shim was used because ByteOS' Makefile calls
  `sudo mount`, while the official image runs as root and has no `sudo`.

Final ByteOS result:

- RV build: `make PLATFORM=riscv64-qemu justbuild` exits 0 and produces
  `target/riscv64gc-unknown-none-elf/release/kernel.bin`.
- RV boot: `make PLATFORM=riscv64-qemu run` reaches `ByteOS:/#`; timeout exit
  124 is expected because QEMU is left at an interactive shell.
- LA build: `make PLATFORM=loongarch64-qemu justbuild` exits 0 and produces
  `target/loongarch64-unknown-none/release/kernel.bin`.
- LA boot: `make PLATFORM=loongarch64-qemu run` reaches `ByteOS:/#`; timeout
  exit 124 is expected because QEMU is left at an interactive shell.

## Decision

Lock `Byte-OS/ByteOS` at commit
`06171525001d73f8660c9fb23b11690b32cefe3d` as the baseline.

M1 must make this durable for contest evaluation:

1. Bring ByteOS into the contest repository with baseline attribution.
2. Record ByteOS license and AI usage in the first attribution commit.
3. Vendor/pin dependencies so builds do not depend on network access.
4. Remove the runtime dependency on host `yq`.
5. Implement `make all` producing `kernel-rv` and `kernel-la`.
