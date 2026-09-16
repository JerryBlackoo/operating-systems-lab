# OS lab GitHub snapshot import status

## Current scope

Only import the GitHub source snapshot from `LiuZhouKang/MtAgentLens` into the existing `JerryBlackoo/operating-systems-lab` repository. Other candidate projects are outside this import attempt. Do not create or delete repositories, remove existing lab content, or rewrite lab history.

- Source: `https://github.com/LiuZhouKang/MtAgentLens.git`
- Pinned source commit: `6465fef610fd7b92dc5c69fcfbc5eab42e969879`
- Intended destination: `research/2026-cscc-os/proj32-comparison/sources/01-mtagentlens/`
- Export ordinary files, recursively flatten pinned submodules, and omit upstream `.git` metadata/history.
- Preserve original source bytes, executable modes, README, LICENSE, NOTICE and authorship information.

## Result: SOURCE IMPORT NOT COMPLETED

The importer and GitHub-only workflow were committed to `main` in `e8bcbcb050c4f49ca929dfa3ca5603cb12f7245d`. This is an importer/configuration commit, **not a source snapshot commit**.

The resulting Actions run `35133995301` failed. Its import job `104921662164` has no executed steps (`steps: []`); no checkout, source clone, export, source commit or source push was performed by that job. The exact provisioning failure reason has not been established from the available connector responses.

Run: https://github.com/JerryBlackoo/operating-systems-lab/actions/runs/35133995301

Do not mark MtAgentLens as vendored until actual source files and the generated `MTAGENTLENS_SNAPSHOT.json` are present in the destination repository. No other candidate sources were imported or deleted by this attempt.

## Importer validation and local execution

`import_mtagentlens.py` passed local fixture tests for exact text/binary bytes, executable mode, symlinks, recursive submodule flattening, omitted Git metadata, exact staged blob hashes, no upstream history, overwrite refusal and external-symlink rejection. These are tests of the importer on synthetic repositories, **not an end-to-end import of MtAgentLens and not tests of the MtAgentLens project**.

The script requires Python 3.10+, Git, GitHub network access and a clean checkout of this lab. From the lab root, run:

`python3 research/2026-cscc-os/proj32-comparison/import_mtagentlens.py`

The standalone script stages the verified snapshot and its provenance record, but does not commit or push. The workflow performs a normal commit/push only after successful export and verification. Existing destination directories and uncommitted work are never silently overwritten. Large files, unresolved LFS pointers and unsafe symlinks cause explicit failure rather than silent omission.

<details>
<summary>Historical five-project import record (superseded scope; retained for provenance)</summary>

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

</details>
