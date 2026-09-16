# proj32 source import status

## Target

Vendor the five public 2026 CSCC OS `proj32` submissions into:

`research/2026-cscc-os/proj32-comparison/sources/`

Git history is intentionally stripped; original README/LICENSE/NOTICE files are to be preserved.

## Current state

- `01-mtagentlens` — **not yet vendored by the new workflow**. Public GitLab + GitHub mirror are available.
- `02-guangming-weilai` — **not yet vendored**. Public GitLab archive is available.
- `03-sange-zhugeliang` — **not yet vendored**. Public GitLab archives are available; a high-match GitHub mirror candidate also exists.
- `04-danji-shuru-wenben` — **not yet vendored**. Public team repository and competition archives are available.
- `05-yuanwei-moli` — **not yet vendored**. Public GitLab archives are available.

## Why the automatic import did not finish

The repository contains `.github/workflows/import-proj32-comparison.yml` with the clone/import logic and all currently known source URLs.

The corrected workflow was accepted by GitHub, but the latest job failed before any step was executed. The Actions API reports:

- `steps: []`
- `runner_id: 0`
- empty `runner_name`

Therefore this failure occurred **before checkout and before any GitLab clone command ran**. It is not evidence that the GitLab repositories themselves are uncloneable. The exact account-side runner provisioning reason is not exposed through the current connector.

The earlier general CSCC import workflow did successfully clone multiple GitLab repositories into this same private GitHub repository, so the vendor approach itself is known to work when a runner is provisioned.

## Known source URLs

### 01 MtAgentLens

- `https://gitlab.eduxiji.net/educg-group-43734-3136859/T202690002999777-3686.git`
- `https://github.com/LiuZhouKang/MtAgentLens.git`

### 02 光明未来

- `https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026900029910646-3462.git`
- final-stage archive family: `proj32-基于eBPF的多智能体异常监测系统-*`

### 03 三个诸葛亮

- `https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026900029911547-2892.git`
- final-stage archive family: `proj32-1-*`
- high-match public mirror candidate: `https://github.com/neko4linux/eBPF-Multi-Agent.git`

### 04 单击输入文本

- `https://gitlab.eduxiji.net/T2026120269910095/project3136859-382916.git`
- `https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026120269910095-3080.git`
- final-stage archive family: `proj32-单击输入文本-*`

### 05 愿为莫离

- `https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026104869910476-3357.git`
- `https://gitlab.eduxiji.net/educg-group-48534-3136859/T2026104869910476-2464.git`
