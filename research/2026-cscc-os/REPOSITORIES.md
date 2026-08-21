# 2026 操作系统设计赛相关仓库清单

> 原始项目主要位于教育系统大赛 GitLab：`https://gitlab.eduxiji.net/`
>
> 这里记录的是本轮对话中明确出现过的项目。对当前无法从公开索引独立确认的细节，用“待核验”标注，不做猜测。

## 1. 上海电力大学 — proj54 Open Euler 内核 step by step

- 队伍：**每天好懵逼地活着**
- Team ID：`T2026102569910321`
- 项目：`proj54-Open Euler内核 step by step`
- 学校：上海电力大学
- 方向：操作系统课程 / 教学实验方向
- 主仓库：
  - https://gitlab.eduxiji.net/T2026102569910321/project3136859-386827
- clone：

```bash
git clone https://gitlab.eduxiji.net/T2026102569910321/project3136859-386827.git
```

### 学习价值

这个项目最适合回答的不是“怎样从零发明一个全新内核”，而是：

```text
一个真实 Linux/openEuler 内核
怎样被拆成可学习、可实验、可逐步修改的课程路径？
```

重点建议看：

- 每一步实验对应修改了什么子系统；
- 如何把大型 Linux 内核拆成教学单元；
- build / boot / debug 环境怎样组织；
- 是否有按阶段递增的实验目标和测试脚本。

---

## 2. 上海电力大学 — sudo_win_the_cscc

- 队伍：`sudo_win_the_cscc`
- Team ID：`T2026102569910192`
- 项目：`OSKernel2026-sudo_win_the_cscc`
- 学校：上海电力大学
- 赛道：内核实现方向
- 仓库：公开搜索可定位，具体稳定 URL 建议以比赛 GitLab Team ID 再核验
- 奖项：**待官方最终名单核验**

### 学习价值

它与 `proj54` 的价值不同：前者偏教学拆解，这个项目更适合观察真正的参赛内核怎样围绕测例推进功能、修 bug 和性能优化。

---

## 3. 华中科技大学 — 干饭不想排队

- 队伍：**干饭不想排队**
- 项目：`proj59-面向嵌入式设备的高效LLM推理内存管理系统`
- 学校：华中科技大学
- 赛道：功能挑战
- 主仓库：
  - https://gitlab.eduxiji.net/T2026104879911351/project3136859-389097

### 可见技术主题

此前公开项目说明可见的关键词包括：

- VMM / 虚拟内存管理；
- 页面淘汰；
- 预取；
- MoE 场景；
- benchmark。

### 学习价值

这是非常好的“OS × AI 系统”交叉项目。阅读时重点追踪：

```text
LLM 推理访问模式
→ 页面冷热变化
→ 淘汰 / 预取策略
→ 缺页和 I/O 行为
→ 延迟与吞吐量
```

---

## 4. 哈尔滨工程大学 — You-Only-Live-Once

- 队伍：**You-Only-Live-Once**
- Team ID：`T2026102179910632`
- 项目：`proj31-You-Only-Live-Once`
- 学校：哈尔滨工程大学
- 赛道：功能挑战
- 国赛归档：
  - https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026102179910632-23

### 学习价值

适合与其他功能挑战项目横向比较：题目目标、基线系统、修改边界、benchmark、最终 demo 分别是怎么设计的。

---

## 5. 大连理工大学 — TraceForce / AgentMem

- 队伍：**TraceForce**
- 项目：`proj47-AgentMem`
- 学校：大连理工大学
- 赛道：功能挑战
- 仓库：比赛 GitLab，可通过项目名 `proj47-AgentMem` / 队名 `TraceForce` 检索
- 奖项：待官方最终名单核验

### 可见技术方向

项目名本身说明其核心在 **Agent memory**。此前公开索引中还可见“真实 agent 调用 demo”等提交信息。

### 学习价值

关注 Agent 长时运行产生的系统问题，而不是只看 agent 应用逻辑：

- memory 生命周期；
- 冷热数据；
- cache / eviction；
- persistent memory；
- 延迟；
- 与 OS 内存子系统的接口。

---

## 6. 武汉纺织大学 — 我去不早说

- 队伍：**我去不早说**
- Team ID：`T2026104959910737`
- 项目：`proj51-我去不早说`
- 学校：武汉纺织大学
- 赛道：功能挑战
- 许可证：此前公开索引显示为 MIT License；建议使用源码前再次在仓库内核验
- 国赛归档：部分页面可能需要登录
- 奖项：待官方最终名单核验

---

# 内核实现方向

## 7. 武汉大学 — 笑南梁队

- 队伍：**笑南梁队**
- Team ID：`T2026104869910817`
- 项目：`OSKernel2026-2026104869910817`
- 学校：武汉大学
- 赛道：内核实现
- 公开归档线索：
  - https://gitlab.eduxiji.net/educg-group-43501-3132633/T2026104869910817-342
- 奖项：此前图片信息指向全国赛优胜奖；最终官方名单仍建议再次核对

### 学习价值

优先查看：

- arch / trap；
- mm；
- task / scheduler；
- syscall；
- fs / VFS；
- final-test 修复历史。

最终代码只是结果，真正有价值的是“哪个测例失败 → 根因在哪个内核抽象 → commit 怎样修掉”。

---

## 8. 武汉大学 — Eureka / Eureka OS

- 队伍：**Eureka**
- 项目：`OSKernel2026-Eureka`
- 内核名：**Eureka OS**
- 学校：武汉大学
- 赛道：内核实现
- 公开项目说明曾明确描述：面向 **RISC-V64** 与 **LoongArch64** 双架构
- 仓库：比赛 GitLab，可用 `OSKernel2026-Eureka` 检索
- 奖项：待官方最终名单核验；此前对话中的图片信息显示其为高水平全国赛项目

### 为什么非常值得拆

“双架构”意味着项目必须主动区分：

```text
architecture-dependent
        vs
architecture-independent
```

这是理解现代 OS 代码组织非常好的入口。

建议对照：

- 两套 arch entry；
- trap / context switch；
- 页表格式；
- timer / interrupt；
- syscall ABI；
- 上层 task/mm/fs 如何保持统一接口。

---

## 9. 合肥工业大学 — 陶酉册

- 队伍：**陶酉册**
- 项目：`2026OS-陶酉册`
- 学校：合肥工业大学
- 赛道：内核实现
- 主仓库：
  - https://gitlab.eduxiji.net/T202510359998209/2026os
- 公开索引此前可见较长开发历史、多 branch / 多 commit
- 奖项：此前图片信息指向全国赛优胜奖；建议以后用官方名单最终核验

### 学习价值

这个项目特别适合看长期工程演进：

- 从能启动到能跑用户程序；
- syscall 数量如何扩展；
- 文件系统和进程语义怎样逐步补齐；
- 测试驱动下反复修边界条件。

---

## 10. 华东师范大学 — 0xDEADBEEF

- 队伍：**0xDEADBEEF**
- Team ID：`T2026102699910462`
- 项目：`OSKernel2026-0xDEADBEEF`
- 学校：华东师范大学
- 赛道：内核实现
- GitLab：可按 Team ID / 项目名检索
- 此前公开提交信息中可见关键词：`final-test`、scheduler state machine、CPU lifecycle、`cyclictest`、VMA 等
- 奖项：此前图片信息指向全国赛优胜奖；最终以官方名单为准

### 为什么非常值得看

这些 commit 关键词说明它已经进入真正的“内核工程”阶段：

```text
不是：实现一个 textbook scheduler
而是：scheduler state machine 在真实边界情况下是否正确
```

重点追踪：

- task state transition；
- SMP / CPU lifecycle；
- VMA merge/split/unmap；
- timer / realtime 相关行为；
- final-test 失败案例。

---

## 11. 山东大学 — LEBRONDIDDY

- 队伍：**LEBRONDIDDY**（此前照片中左下队名被部分遮挡，匹配度高）
- Team ID：`T2026104229910571`
- 学校：山东大学
- 赛道：内核实现
- 比赛归档：
  - https://gitlab.eduxiji.net/educg-group-43501-3132633/T2026104229910571-2751
- 归档名线索：`OSKernel2025-X-2751`
- 身份匹配状态：**较高概率，但仍建议以比赛页面最终核验照片与 Team ID 的对应关系**

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

# 重要说明：commit 数不能直接等于“自己写了多少代码”

例如一个仓库显示 1770 commits，可能同时包含：

- 上游基础项目历史；
- merge commits；
- 自动同步；
- 多人协作；
- 小步提交；
- 比赛平台归档迁移。

判断队伍真正工作量应该看：

```bash
git merge-base BASE FINAL

git diff --stat BASE..FINAL
git shortlog -sne BASE..FINAL
git log --first-parent BASE..FINAL
```

再结合目录级 diff，才能回答“他们真正修改了什么”。
