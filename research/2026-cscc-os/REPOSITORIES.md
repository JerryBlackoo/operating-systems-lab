# 2026 操作系统设计赛相关仓库清单

> 原始项目主要位于教育系统大赛 GitLab：`https://gitlab.eduxiji.net/`
>
> 这里记录的是本轮对话中明确出现过的项目。对当前无法从公开索引独立确认的细节，用“待核验”标注，不做猜测。

## 上海电力大学

### 1. proj54 Open Euler 内核 step by step

- 队伍：**每天好懵逼地活着**
- Team ID：`T2026102569910321`
- 项目：`proj54-Open Euler内核 step by step`
- 学校：上海电力大学
- 方向：操作系统课程 / 教学实验方向
- 主仓库：https://gitlab.eduxiji.net/T2026102569910321/project3136859-386827
- clone：

```bash
git clone https://gitlab.eduxiji.net/T2026102569910321/project3136859-386827.git
```

最值得看：如何把 Linux/openEuler 大型内核拆成 build、boot、debug、子系统实验等逐步教学单元。

### 2. sudo_win_the_cscc

- 队伍：`sudo_win_the_cscc`
- Team ID：`T2026102569910192`
- 项目：`OSKernel2026-sudo_win_the_cscc`
- 学校：上海电力大学
- 赛道：内核实现
- 奖项：**待官方最终名单核验**

价值：适合观察真正参赛内核如何围绕测例推进功能、兼容性和 bug 修复。

---

## 功能挑战方向

### 3. 华中科技大学 — 干饭不想排队

- 项目：`proj59-面向嵌入式设备的高效LLM推理内存管理系统`
- 主仓库：https://gitlab.eduxiji.net/T2026104879911351/project3136859-389097
- 可见主题：VMM、页面淘汰、预取、MoE、benchmark。

阅读主线：

```text
LLM 推理访问模式
→ 页面冷热变化
→ 淘汰 / 预取策略
→ 缺页与 I/O
→ 延迟 / 吞吐量
```

### 4. 哈尔滨工程大学 — You-Only-Live-Once

- Team ID：`T2026102179910632`
- 项目：`proj31-You-Only-Live-Once`
- 国赛归档：https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026102179910632-23

价值：适合横向比较功能挑战项目的题目目标、基线系统、修改边界、benchmark 和 demo。

### 5. 大连理工大学 — TraceForce / AgentMem

- 队伍：`TraceForce`
- 项目：`proj47-AgentMem`
- 学校：大连理工大学
- 赛道：功能挑战
- 稳定 clone URL：待核验

重点看：Agent 长时运行产生的 memory 生命周期、冷热数据、cache / eviction、持久化与延迟问题。

### 6. 武汉纺织大学 — 我去不早说

- Team ID：`T2026104959910737`
- 项目：`proj51-我去不早说`
- 赛道：功能挑战
- 许可证：此前公开索引显示 MIT License，使用前应再次在仓库内确认
- 奖项：待官方最终名单核验

---

## 内核实现方向

### 7. 武汉大学 — 笑南梁队

- Team ID：`T2026104869910817`
- 项目：`OSKernel2026-2026104869910817`
- 归档：https://gitlab.eduxiji.net/educg-group-43501-3132633/T2026104869910817-342
- 此前图片信息指向全国赛优胜奖，最终仍建议以官方名单核验

优先看：arch/trap、mm、task/scheduler、syscall、fs/VFS、final-test 修复历史。

### 8. 武汉大学 — Eureka / Eureka OS

- 项目：`OSKernel2026-Eureka`
- 内核名：`Eureka OS`
- 学校：武汉大学
- 赛道：内核实现
- 公开说明曾明确描述：面向 **RISC-V64** 与 **LoongArch64** 双架构
- 稳定 clone URL：待核验
- 奖项：待官方最终名单核验

最值得学的是：

```text
RISC-V64 ─┐
           ├── arch abstraction ──> common kernel
LoongArch ─┘
```

即架构相关代码与通用内核代码的边界。

### 9. 合肥工业大学 — 陶酉册

- 项目：`2026OS-陶酉册`
- 主仓库：https://gitlab.eduxiji.net/T202510359998209/2026os
- 赛道：内核实现
- 此前图片信息指向全国赛优胜奖，最终以官方名单为准

价值：特别适合看长期迭代、测例驱动、syscall/VFS/进程语义逐步补齐的过程。

### 10. 华东师范大学 — 0xDEADBEEF

- Team ID：`T2026102699910462`
- 项目：`OSKernel2026-0xDEADBEEF`
- 赛道：内核实现
- 公开提交信息曾出现：`final-test`、scheduler state machine、CPU lifecycle、`cyclictest`、VMA
- 奖项：此前图片信息指向全国赛优胜奖，最终以官方名单为准

重点：task 状态机、SMP/CPU lifecycle、VMA merge/split/unmap、timer/realtime、final-test 边界修复。

### 11. 山东大学 — LEBRONDIDDY

- Team ID：`T2026104229910571`
- 学校：山东大学
- 赛道：内核实现
- 国赛归档：https://gitlab.eduxiji.net/educg-group-43501-3132633/T2026104229910571-2751
- 归档名线索：`OSKernel2025-X-2751`
- 身份匹配状态：照片中队名被遮挡，匹配度高，但仍建议最终核验

---

# 横向分类

| 项目 | 类型 | 最值得学什么 |
|---|---|---|
| Open Euler step by step | 教学/课程 | 怎样把 Linux 内核拆成逐步实验 |
| sudo_win_the_cscc | 内核实现 | 参赛内核完整工程流程 |
| proj59 LLM memory | 功能挑战 | LLM 推理 × OS 内存管理 |
| You-Only-Live-Once | 功能挑战 | 完整功能挑战工程结构 |
| AgentMem | 功能挑战 | Agent memory × 系统机制 |
| proj51 | 功能挑战 | 功能挑战项目工程化 |
| 笑南梁队 | 内核实现 | kernel 子系统完整性 |
| Eureka OS | 内核实现 | 双架构抽象、可移植性 |
| 陶酉册 | 内核实现 | 长周期迭代和测例驱动 |
| 0xDEADBEEF | 内核实现 | 调度/VMA/SMP/边界修复 |
| LEBRONDIDDY | 内核实现 | 全国赛内核项目对照样本 |

# commit 数不能直接等于“自己写了多少代码”

1770 commits 可能同时包含上游历史、merge、自动同步、多人协作和小步提交。判断真实工作量应找基线并比较：

```bash
git merge-base BASE FINAL
git diff --stat BASE..FINAL
git shortlog -sne BASE..FINAL
git log --first-parent BASE..FINAL
```

再结合目录级 diff，才知道队伍真正新增和修改了什么。
