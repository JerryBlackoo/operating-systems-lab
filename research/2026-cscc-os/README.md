# 2026 全国大学生计算机系统能力大赛：操作系统设计赛项目索引与一等奖总结

> 整理日期：2026-08-21
>
> 本目录用于集中整理 2026 全国大学生计算机系统能力大赛操作系统设计赛相关公开项目，重点关注 **内核实现赛道**、**功能挑战赛道**以及课程/教育方向项目。
>
> 注意：部分项目在教育系统比赛 GitLab（`gitlab.eduxiji.net`）而不是 GitHub。本仓库不复制第三方源码，避免破坏原始 Git 历史和许可证边界；这里保存项目索引、学习分析和克隆入口。

## 目录

- [`REPOSITORIES.md`](./REPOSITORIES.md)：本次对话涉及到的全部项目、队伍、学校、赛道与链接
- [`FIRST_PRIZE_SUMMARY.md`](./FIRST_PRIZE_SUMMARY.md)：2026 国赛一等奖/高水平决赛项目的工程能力总结
- [`LEARNING_PATH.md`](./LEARNING_PATH.md)：如何从这些项目反推一条自己的 OS 内核学习路线
- [`clone-public-repos.sh`](./clone-public-repos.sh)：一键克隆目前已明确公开 clone 地址的项目

## 这批项目应该怎么看

不要只按“代码量”“commit 数”评价项目。OS 国赛真正有价值的维度更接近下面这条链：

```text
能启动
  ↓
能处理中断 / 异常 / syscall
  ↓
能做虚拟内存
  ↓
能调度多进程 / 多线程
  ↓
能跑文件系统
  ↓
能兼容更多 syscall / 用户程序
  ↓
能在真实测试压力下稳定运行
  ↓
性能、可移植性、多架构、工程化
```

因此阅读时建议始终回答六个问题：

1. **它基于什么起点？** 自研内核、Linux/openEuler 改造，还是教学 OS？
2. **启动链是什么？** bootloader/firmware → entry → MMU → kernel main 的路径在哪里？
3. **核心抽象怎么实现？** task、address space、VFS、file、inode、scheduler 分别是什么数据结构？
4. **系统调用怎么走？** 用户态 trap → 内核入口 → syscall dispatch → 子系统 → 返回。
5. **项目真正难点是什么？** “写出功能”还是“通过复杂兼容/性能测试”？
6. **哪些部分是他们真正新增的？** 一定要看 commit/diff，而不是只看仓库最终总代码量。

## 当前收录范围

本轮整理包含此前对话中出现的这些项目/队伍：

- 上海电力大学：`proj54-Open Euler内核 step by step`
- 上海电力大学：`sudo_win_the_cscc / OSKernel2026-sudo_win_the_cscc`
- 华中科技大学：`干饭不想排队 / proj59-面向嵌入式设备的高效LLM推理内存管理系统`
- 哈尔滨工程大学：`You-Only-Live-Once / proj31`
- 大连理工大学：`TraceForce / proj47-AgentMem`
- 武汉纺织大学：`我去不早说 / proj51`
- 武汉大学：`笑南梁队 / OSKernel2026`
- 武汉大学：`Eureka / Eureka OS`
- 合肥工业大学：`陶酉册 / 2026OS-陶酉册`
- 华东师范大学：`0xDEADBEEF / OSKernel2026-0xDEADBEEF`
- 山东大学：`LEBRONDIDDY`

详细状态见 [`REPOSITORIES.md`](./REPOSITORIES.md)。

## 关于“2026 一等奖”

截至整理时间 **2026-08-21**，公开搜索索引和部分比赛页面的信息并不完全同步。因此本仓库将：

- 已有明确公开证据的奖项直接写明；
- 只有“进入全国赛/决赛”“优胜奖/三等奖”等信息的，不升级成一等奖；
- 暂未找到独立公开证据的，一律标为 **待官方名单核验**；
- `FIRST_PRIZE_SUMMARY.md` 的重点是总结一等奖级项目通常体现出来的 **技术与工程能力**，而不是在证据不足时凭印象给队伍重新排名。

## 最值得优先拆的内核项目

如果目标是学会“真正做 OS”，优先顺序建议：

```text
Eureka OS / 0xDEADBEEF
        ↓
笑南梁队 / 陶酉册
        ↓
sudo_win_the_cscc
        ↓
Open Euler step by step（教学路线）
```

原因不是简单的奖项高低，而是前几类项目更适合研究：启动、内存管理、调度、系统调用、VFS、兼容性测试和多架构适配；`Open Euler step by step` 则更适合按教学顺序理解内核机制。
