# PLAN — OS 内核实现赛 · 主计划与衔接锚

> 这是精简工作流后的**唯一活跃主文档**。新会话/新模型接手先读它 + `docs/PLAYBOOK.md`（硬核环境教训）+ `docs/oj-scores.csv`（分数史）。
> 最近更新：2026-06-18（LA LTP rootcause 后；补齐 OJ 忠实测法前置）。

## 0. 工作流（已精简；2026-06-17 远端从 WSL 迁到课题组服务器，WSL 已退役）

- **Claude**：定策略、选下一个目标、写验收标准、把关红线。
- **本机 Codex（goal 模式）**：在 Mac 上实现代码。所有编程在本机进行。
- **远端构建/回归服务器**（`ssh yxzhang@10.102.35.44`，Ubuntu x86_64，**80 核 / 251GB**）：**只跑回归/模拟**，用官方镜像 `zhouzhouyi/os-contest:20260510`。一次性 bootstrap 见 §0.5。
- **代码送到远端**：Mutagen `oskernel-school-sync`（Mac `/Users/.../内核实现赛` ↔ 服务器 `/home/yxzhang/oskernel2025-x`）。**改完代码 Mutagen 自动同步,无需手动 scp。**
- **OJ 提交永远是人点。** 服务器无 GitLab 凭据，`git push` 从 Mac。
- ⛔ 旧 WSL（`lenovo@100.92.143.96`）**已弃用、不可达**——文档/脚本里凡指向它的一律改服务器,勿再用。

## 0.5 服务器一次性 bootstrap（2026-06-17 首次启用）

服务器已装 Docker(80c/251GB)、Mutagen 同步已通,**唯一缺**官方镜像——服务器**连不上 Docker Hub**(`registry-1.docker.io` 超时,国内网络墙)。把官方镜像弄上去,任选一条可行的:

- **A) 从能拉镜像的机器 `save|load` 传**（M0 验证过的路）：在该机器上
  `docker pull --platform linux/amd64 zhouzhouyi/os-contest:20260510` →
  `docker save zhouzhouyi/os-contest:20260510 | ssh yxzhang@10.102.35.44 'docker load'`（走内网,~几分钟）。
- **B) 配国内 registry mirror**：服务器 `/etc/docker/daemon.json` 加 `{"registry-mirrors":["<可用镜像源>"]}`（校内/aliyun…）→ `sudo systemctl restart docker` → `docker pull zhouzhouyi/os-contest:20260510`。
- **C) 已有 tar**：`docker load -i <tar>`。

验证：`ssh yxzhang@10.102.35.44 'docker images | grep os-contest'` 出现该镜像即 ready。

**sdcard 忠实测法要用服务器 staged 官方 4G 镜像**：
`/home/yxzhang/oscomp-sdcard/sdcard-rv.img`、
`/home/yxzhang/oscomp-sdcard/sdcard-la.img`。repo 根下
`/home/yxzhang/oskernel2025-x/sdcard-{rv,la}.img` 是 96M 小镜像/旧快测镜像，不能作为 OJ 忠实全量或 LA-LTP 证据。

## 1. 现状坐标（2026-06-19）

- 分支 `main` @ direct `unlinkat("")` fix merge after `e206a19` busybox-bank；`origin/main` may lag until pushed.
- Tags：`oj-712-floor`、`oj-1296-floor`、`oj-glibcrv-floor`(=0d08d56)、`oj-busybox-bank-20260619`(=OJ 1508 candidate).
- **分数态势**：
  - banked 高分 **1508.5793**（OJ 2026-06-19，candidate `e206a19` / tag `oj-busybox-bank-20260619`），见 `review_packets/2026-06-19-OJ1508-busybox-bank-result.md`。这是当前受保护诚实高分：basic 408、busybox 214、libctest 219、LTP 2592、lua 36。
  - 旧 `oj-1296-floor` 仍保留，但已被 1508 诚实分覆盖；其 LA LTP garbage overflow 不再作为目标路线。
- **TASK-024 已落地真增益**（官方镜像、clean build、实测）：rv-glibc 原本 0 分整列，现 busybox-glibc 55/55、lua-glibc 9、libctest-musl 190 Pass!、libcbench/iozone/lmbench 均真跑到 END。basic-4 四象限全绿。
- **BusyBox bank 已落地 OJ 真增益**：`sys initproc` BusyBox per-child watchdog `10s -> 60s`，OJ busybox row `96 -> 214`，总分 `1389.143 -> 1508.579`。
- **LA-LTP rootcause 已落地诊断证据**：见 `review_packets/2026-06-18-LA-LTP-rootcause.md`。忠实复现必须是单架构、全 12 组、单一全局预算、官方 4G staged sdcard。结论：`musl-la` 是全局预算截断，不是崩溃；本地忠实跑已进入 `ltp-musl`，到 `fs_bind_rbind20.sh` 前有 `626` runs / `150 returned 0`，随后 LA kernel global deadline 强制关机。`glibc-la` 本地忠实跑完整结束 `2840/2840`，`226 returned 0`，不是本地 LTP 0 或 sdcard 不一致。
- **当前待验证候选**：direct-bin-walk `unlinkat(AT_FDCWD, "", AT_REMOVEDIR) -> ENOENT` 修复已合本地 `main`，目标是避免 LTP helper `rmdir ""` 删除 `/glibc`。已有 focused/basic/busybox gates；尚未 OJ 验证。`ae5a826` manifest runner 仍禁止合 main。

## 1.5 LA-LTP Day2 前置条件（接力必读）

1. **先读证据包**：`review_packets/2026-06-18-LA-LTP-rootcause.md` 是 Day2 的事实基线。不要再用 LTP-only 6600s 或 repo 根 96M 镜像解释 OJ 的 LA 分数。
2. **分支/工作树**：从 `main` 开 `wip/la-ltp-<日期>`。开始前确认 Goal1 packet 已保留；若工作树有无关脏改，先停下确认，不要顺手覆盖。
3. **远端与镜像**：回归只在 `ssh yxzhang@10.102.35.44` 的 `/home/yxzhang/oskernel2025-x` 跑；Docker image 是 `zhouzhouyi/os-contest:20260510`；忠实全量挂载 `/home/yxzhang/oscomp-sdcard:/sdcards:ro`，`TEST_IMAGE=/sdcards/sdcard-{rv,la}.img`。
4. **忠实验收尺子**：单架构单 QEMU、全组 sdcard、外层约 `7200s`，内核当前 `GLOBAL_DEADLINE_SECS=6600`。可用 targeted run 定位，但成功只能用忠实全组预算证明。
5. **Day2 主攻方向**：这是预算/长尾问题，不是 LA LTP 硬崩。优先减少 LA 预算消耗：`libcbench-musl` 600s、`lmbench-glibc` 540s、`ltp-glibc` 3451s，以及 LTP 124 长尾（`cpuset*`、`cpuhotplug*`、`cgroup*`、`dns*`、`ftrace*` 等）。允许让本来挂死/本来 0 的 case 在内核侧真实快速失败；不允许跳测、伪造、改弱 harness 或把能过 case 改错。
6. **成功口径**：真实 `returned 0` 只按 `FAIL LTP CASE ... : 0` / guest `returned 0` 统计；同时记录组起止时间、LTP 进度、最后 case、是否触发 kernel global deadline/host timeout。basic-4 非回归和 `kernel-rv` soft-float 门仍强制。

## 2. 宏观路线（终态 + 里程碑；2026-06-16 拨乱反正定稿）

**终态（执行到最后的任务）**：06-30 初赛截止前，交出一个 ①**不被取消资格**（设计文档 + AI 使用声明 + ≥8 带注释提交 + 许可合规）、②诚实分尽可能高（力争 honest > 1296.25，**最差有 `oj-1296-floor` 兜底**）、③basic-4 绿且不依赖 garbage 的初赛作品。

**门（每步不变）**：clean build → `file kernel-rv`=soft-float → basic-4 不回归（RV ≥240s）→ 实测 delta 为正。**绝不灌 garbage、绝不伪造 harness。**

- **M0 稳定 & bank（已做）**：`wip/ltp-reach-20260616` 已存；`main=9ff4f91`（TASK-024 诚实增益 + 清理）；`oj-1296-floor` 兜底安全。
- **M1 锁住"必交 + floor"（最高确定性，先做，与 M2 并行）**：设计方案文档 + **AI 使用声明（赛规强制，缺=判违规）** + ≥8 次带注释提交 + 许可合规。**"不做会亏"、零技术风险，优先级高于再抠分。**
- **M2 高效诚实拱分（批量战役）**：用 la-musl 已走完 2820 的**完整失败清单**，把所有 fail 按【根因簇 × 修复成本】分类 →
  **批量修（一批改多处、一次 build、targeted 验证），全量只做周期性测量**（不当逐次尺子）→
  顺序：便宜单行先 → 高乘数根因（缺的 syscall 族 / procfs 缺口…）→ 贵功能（cgroup/namespace）按 ROI 决定投不投。穿插 perf（lmbench/iozone/libcbench）调优。
- **M3 里程碑测量 + 提交决策**：里程碑用全量 / **OJ 当 oracle**（它必然跑完）测真分。honest 稳 > 1296.25 → 写 GO 版 `OJ-READY.md` → 人点；若明显卡在 1296 下 → 交 floor 兜底。
- **M4 收尾**：定稿文档、最终提交（人点 OJ）。

> 批量原则（成本控制）：**批的是"跑"不是"修"**——一次 build 验多个修复能省时间；但每个独立 feature 的实现成本不会因批量而消失，按根因簇批量才有乘数。

## 3. 铁律（红线）

1. **basic-4 非回归强制**：rv-glibc/rv-musl/la-glibc/la-musl 四象限 basic 必须全绿。任一退化 → 停，不许合 main。
2. **真实输出**：绝不伪造、绝不削弱/跳过/绕过 harness；LTP 只认真 TPASS/真整例，**不认 garbage TBROK 溢出**。
3. **OJ 人点**：写好 `OJ-READY.md` + 通知即可，绝不自动提交。
4. **保护已 banked 提交**：`oj-1296-floor`(kernel `116cdcf8`) 是受保护的提交锚，别覆盖；新工作产生新 kernel 树是预期内的。
5. **假阴性铁律**：增量编译会悄悄链旧 hard-float 对象、产出"看着 soft-float 实则崩 139"。任何信任的跑之前先 `file kernel-rv` 确认 `soft-float`；存疑 `rm -rf kernel/target` 重编。
6. **Codex 执行纪律**：不悄悄扩范围、不做架构/baseline 决策、同一测试失败两次或涉及 >3 模块或触及 license/测点解读不清 → 升级给 Claude/人。
7. **雷区**：不要碰 cancel-signal-futex 路径（历史上 RV basic 712→289 崩盘，`wip/quarantine-rvcancel-91eb559`）。

## 4. 构建 & 回归 recipe（远端服务器 `yxzhang@10.102.35.44`，官方镜像）

> 路径：仓库 `/home/yxzhang/oskernel2025-x`(Mutagen 自动同步)；OJ 忠实/全量 sdcard 测试镜像 `/home/yxzhang/oscomp-sdcard/`(§0.5 一次性 staging)。
> **`make clean` 是坏的**（顶层没传 `PLATFORM`）。干净构建用 `rm -rf kernel/target`：

```bash
# 干净构建（双架构）
docker run --rm --network none -e RUSTUP_TOOLCHAIN=nightly-2025-01-18 \
  -v /home/yxzhang/oskernel2025-x:/workspace -w /workspace zhouzhouyi/os-contest:20260510 \
  bash -lc 'rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; make -B all; \
            file kernel-rv kernel-la; readelf -h kernel-rv | grep -i flags'
# 期望：kernel-rv = "RVC, soft-float ABI"；kernel-la = LoongArch

# basic-4 门（RV 慢，给够 ≥240s；可用 repo 根 96M quick/basic 镜像，run-la.sh 同理）
docker run --rm --network none \
  -v /home/yxzhang/oskernel2025-x:/workspace \
  -w /workspace \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=240 TEST_IMAGE=/workspace/sdcard-rv.img TEST_IMAGE_LAYOUT=nested \
            EXPECTED_GROUPS=basic-glibc,basic-musl bash harness/run-rv.sh'   # run-la.sh 同理

# 全量/OJ 忠实回归：单架构单 QEMU、官方 4G sdcard fresh copy、全 12 组、外层约 7200s。
# 当前内核 global deadline 为 6600s；从 transcript 自己逐组统计（不要信 harness 最后那行判定）。
# 只设 EXPECTED_GROUPS=basic-* 不会阻止 full image 继续跑后续组；basic-only gate 要用 96M quick 或 cropped disposable image。
```

**逐组打分器**（去 CRLF，按 group 计 success/Pass!）：
```awk
{ sub(/\r$/,"") }
/#### OS COMP TEST GROUP START / { g=$7; next }
/#### OS COMP TEST GROUP END / { g=""; next }
g!="" && /success$/ { s[g]++ }  g!="" && /^Pass!$/ { p[g]++ }
END { for (k in s) print k, "success="s[k]; for (k in p) print k, "Pass!="p[k] }
```
LTP 计数：`grep -c "RUN LTP CASE" / "TPASS" / "TFAIL" / "TBROK"`（TBROK 海量=单例刷屏 garbage，非真过）。

### 4.1 远端评估规则 / 镜像来源 / 并发策略（2026-06-18 audit 固化）

- 证据包：`review_packets/2026-06-18-remote-eval-audit.md`。服务器 Docker image id =
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`；
  当前镜像内 QEMU 是 `10.0.2`，而比赛 README 记录 `9.2.1`，所有 OJ-like 结论都要带这个 caveat。
- full/OJ-like 只认 `/home/yxzhang/oscomp-sdcard/sdcard-{rv,la}.img` 的 4G staged 镜像 fresh disposable copy；repo 根 96M `sdcard-{rv,la}.img` 只用于 basic/quick。
- full/OJ-like 禁止 `.ltp_case_limit`、删 testcode、裁剪 payload、伪造 `returned0`；必须记录 commit、kernel sha/file、sdcard sha、Docker image id、QEMU version、完整命令、log 路径、每组起止/END marker、最后 case、returned0 统计、global deadline/host timeout 原因。
- staged master image 不跑 `e2fsck`；只对 disposable copy 跑 `e2fsck -fy`。远端 `cp --reflink=always` 不支持，copy 用 `cp --reflink=auto`（当前会退化成普通 copy）。
- QEMU 只能 TCG、容器无 `/dev/kvm`；忠实跑不改 `-smp 1`。targeted/window run 最多 3-4 个并发且每个独立 sdcard copy；full/OJ-like 最多 RV+LA 两个并发；LA timing/rootcause 单跑。
- 暂不把 build gate 改成 `make -j$(nproc)`；若探索并行构建，仍需用当前 clean build + `file/readelf` + basic-4 验证后才可信。

## 5. 已知问题（P1 入口）

- **LA `ltp-musl` 预算截断**：忠实全量复现已进入 LTP 并有真实 `150 returned 0`，但在 `fs_bind_rbind20.sh` 附近触发 LA kernel global deadline。Day2 入口是减少预算消耗/让挂死 case 真实快速失败。
- **LA `ltp-glibc` 本地并非硬墙**：忠实全量复现完整结束 `2840/2840`，`226 returned 0`。若 OJ 仍异常，优先按全局终止后的聚合/计分差异校准，不按本地崩溃处理。
- **sdcard 假证据风险**：repo 根下 96M `sdcard-{rv,la}.img` 不是忠实全量镜像。LA-LTP/OJ-like 证据只认 `/home/yxzhang/oscomp-sdcard/` 4G staged 官方镜像。

## 6. 知识与证据指针

- 分数史：`docs/oj-scores.csv`
- 证据包：`review_packets/`（含 `2026-06-15-TASK-024-softfloat-full-regression.md`、`2026-06-14-TASK-027-ltp-sweep-batch1.md`）
- 硬核环境教训：`docs/PLAYBOOK.md`；决策史：`DECISION_LOG.md`
- TASK-024 FPU 根因（落地修复的原理，设计文档素材）：`docs/task024-fpu-findings.md`
- 远端留底原始回归日志：`~/oskernel-artifacts/full-{rv,la}.log.gz`、`mygate-*.log.gz`
- 提交闸门状态：`OJ-READY.md`（当前 HOLD）
- 远端执行/同步细节：`REMOTE_EXECUTION.md`；提交流程：`GIT_WORKFLOW.md`；赛题/测点：`contest_capture/`
