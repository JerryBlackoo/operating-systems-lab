# OpenEuler 操作系统实验项目

> **赛题编号**：proj54 - OpenEuler 系统实践 Step by Step
> **赛道**：功能挑战赛道
> **参赛学校**：上海电力大学
> **指导教师**：徐曼

---

## 核心文档导航

| 文档 | 用途 | 路径 |
|------|------|------|
| **实验设计文档** | 完整设计思路、四层递进体系、各实验概述、技术创新点 | [实验设计文档.md](./实验设计文档.md) |
| **更新日志** | 项目开发全过程记录、Git阶段性提交建议 | [更新日志.md](./更新日志.md) |
| **非本队来源说明** | 开源引用清单、许可证、修改说明 | [非本队来源说明.md](./非本队来源说明.md) |
| **各实验文档** | 实验指导书、设计方案、测试报告、电力场景 | [doc/](./doc/) |

> **首次了解本项目**：请先阅读 [实验设计文档.md](./实验设计文档.md)，包含完整的项目定位、四层递进体系、九大实验设计、代码框架、研发问题与解决方案、技术创新点等内容。

---

## 一、项目概述

本项目基于 **openEuler 24.03 LTS** 操作系统，完成了 9 个内核实验，涵盖操作系统核心子系统：

- **环境配置**：虚拟机安装、桌面环境配置
- **内核模块编程**：字符设备驱动、参数传递
- **进程管理**：进程通信、调度算法
- **内存管理**：内核内存分配、地址转换
- **文件系统**：虚拟磁盘、inode 管理
- **网络管理**：Socket、Netfilter、XDP
- **容器与调优**：iSulad、A-Tune

**四大教育创新点**：
- **三阶段递进式教学设计**：实验二（CP1/CP2/CP3）和实验八（Phase1/2/3）将复杂任务拆解为可独立验证的阶梯式子任务，降低学习曲线
- **Starter + Solution 双轨代码脚手架**：每个核心实验提供学生填空版（带TODO注释）和完整参考版，老师好教学生好学
- **用户态模拟降低学习门槛**：实验七用纯C语言在用户态模拟文件系统，无需内核环境即可理解inode/位图/目录等核心原理
- **电力行业场景深度融合**：结合上海电力大学行业背景，在实验二/三/四/六/八/九中融入电力监控、电网调度、智能变电站、电力协议过滤等真实场景

---

## 二、实验列表

| 实验编号 | 实验名称 | 核心内容 | 负责人 |
| --- | --- | --- | --- |
| 1 | 环境搭建 | VMware 虚拟机创建、OpenEuler 安装、测试脚本 | **金奕如** |
| 2 | 内核模块编程 | 基础命令、3个 checkpoint（加载/参数/字符设备） | **宋雅笛** |
| 3 | 进程通信 | Pipe、消息队列、共享内存、生产者-消费者 | **邵奕** |
| 4 | 调度平台 | FCFS/SJF/RR/CFS 四种调度算法、plot.py 绘图 | **金奕如** |
| 5 | 内存管理 | 虚拟地址、分页机制、大页性能对比 | **邵奕** |
| 6 | 设备管理 | 字符设备驱动、电力传感器驱动扩展 | **金奕如** |
| 7 | 文件管理 | 虚拟磁盘、超级块、inode、目录结构 | **宋雅笛** |
| 8 | 网络+eBPF | UDP Socket、Netfilter、XDP 高性能过滤 | **宋雅笛** |
| 9+10 | 容器+A-Tune | iSulad 容器运行、A-Tune 智能调优 | **邵奕** |

---

## 三、团队成员与分工

| 姓名 | 角色 | 负责实验 |
|------|------|----------|
| 金奕如 | 开发者 | 实验1、4、6 |
| 宋雅笛 | 开发者 | 实验2、7、8 |
| 邵奕 | 开发者 | 实验3、5、9|
| 徐曼 | 指导教师 | 全部 |

---

## 四、技术覆盖范围

### 用户态技术

| 技术 | 应用实验 |
|------|----------|
| Linux 基础命令 | 实验二 |
| GCC 编译、Makefile | 全部 |
| Socket 编程 (UDP) | 实验八 |
| Python + Matplotlib | 实验四 |
| iSulad 容器引擎 | 实验九 |

### 内核态技术

| 技术 | 应用实验 |
|------|----------|
| 内核模块开发 | 实验二、五、六、八 |
| `module_param` 参数传递 | 实验二 |
| 字符设备驱动、`file_operations` | 实验二、六 |
| `kmalloc` / `vmalloc` | 实验五 |
| Netfilter 钩子 | 实验八 |
| eBPF / XDP | 实验八 |

---

## 五、实验环境

| 项目 | 配置 |
| --- | --- |
| 操作系统 | openEuler 24.03 LTS |
| 内核版本 | 6.6.0-132.0.0.111.oe2403sp3.x86_64 |
| 虚拟化平台 | VMware Workstation Pro 17 |
| 编译器 | GCC 12.3.1 / Clang 15.0.7 |
| 构建工具 | Make 4.4.1 |
| 其他依赖 | kernel-devel, python3, matplotlib, iSulad, A-Tune |

---

## 六、关键成果摘要

### 实验1：环境搭建（金奕如）
- 成功创建 openEuler 虚拟机，安装 DDE 桌面环境
- 完成网络配置、系统更新、kernel-devel 安装
- 提供 test_env.sh 自动化环境检查脚本

### 实验2：内核模块编程（宋雅笛）
- 实现 3 个递进的 Checkpoint（基础加载 → 参数传递 → 字符设备）
- 字符设备驱动 `/dev/power_device` 支持读写电力数据
- 功率自动计算：`power = voltage × current`

### 实验3：进程通信（邵奕）
- 实现 Pipe、消息队列、共享内存三种 IPC 机制
- 完成生产者-消费者模型，支持电力场景负载
- 预留 eBPF 检测接口供实验8复用

### 实验4：调度平台（金奕如）
- 实现 FCFS/SJF/RR/CFS 四种调度算法
- 支持 CPU 密集型、I/O 密集型、混合型三种负载集
- 自动生成性能对比柱状图（周转时间、响应时间）

### 实验5：内存管理（邵奕）
- 实现 kmalloc/vmalloc 内核模块，验证物理地址连续性
- 完成分页机制模拟和地址转换
- 大页性能对比测试

### 实验6：设备管理（金奕如）
- 字符设备驱动 `/dev/char_device` 注册成功
- power_sensor 扩展支持电压/电流/温度传感器切换

### 实验7：文件管理（宋雅笛）
- 用户态文件系统模拟器，无需内核依赖
- 支持 512 个文件、最大 8KB 单文件
- 实现 inode、目录项、位图管理，提供命令行交互

### 实验8：网络+eBPF（宋雅笛）
- 三阶段实现：Socket → Netfilter → XDP
- Netfilter 拦截 UDP 40000 端口数据包
- XDP 性能达 8M+ PPS，验证电力协议过滤可行性

### 实验9+10：容器+A-Tune（邵奕）
- iSulad 成功拉取镜像并运行容器
- A-Tune 调优模板测试，验证性能影响
- 容器化电力应用场景验证

---

## 七、文档清单

### 顶层文档

| 文档类型 | 路径 | 说明 |
|---------|------|------|
| 项目总览 | [README.md](./README.md) | 本文档 |
| **实验设计文档** | [实验设计文档.md](./实验设计文档.md) | 完整设计思路、四层递进、技术创新 |
| 更新日志 | [更新日志.md](./更新日志.md) | 开发全过程、Git阶段性提交建议 |
| 非本队来源说明 | [非本队来源说明.md](./非本队来源说明.md) | 开源引用、许可证、修改说明 |

### 各实验文档（位于 [doc/](./doc/) 目录下）

全套9个实验均配有完整的文档体系，统一包含以下内容：

- **实验指导书.md**：详细操作步骤、分阶段检查点、常见问题排查 — 所有实验均已提供
- **设计方案.md**：技术架构、核心数据结构、关键代码说明 — 核心实验已提供完整版
- **测试报告.md**：功能测试、性能测试、对比分析 — 核心实验已提供完整版
- **电力场景.md**：电力行业应用场景深度解读 — 实验二/七/八已提供独立文档，其余实验的电力场景融入实验指导书

每个实验的 `src/` 目录下统一采用 **Starter + Solution 双轨结构**：
- `starter/`：学生填空代码框架（带TODO注释）
- `solution/`：完整参考实现
- `tests/`：自动化测试脚本（test_*.sh，输出PASS/FAIL）
- 实验二/七/八额外提供 `deploy.sh` 一键自动部署脚本

---

## 八、快速开始

### 一键自动部署（推荐）

实验二/七/八提供了一键自动部署脚本 `deploy.sh`，可自动完成环境检查、编译、加载、功能验证和测试：

```bash
# 实验二：内核模块（电力设备驱动）
cd src/实验二 && sudo ./deploy.sh              # 默认 solution 版
cd src/实验二 && sudo ./deploy.sh starter      # 学生填空版

# 实验七：用户态文件系统（电力数据存储）
cd src/实验七 && ./deploy.sh                   # 无需 root

# 实验八：网络管理 eBPF/XDP（电力协议过滤）
cd src/实验八 && sudo ./deploy.sh
```

### 手动操作

各实验源码位于 `src/` 目录下，按实验编号组织。以下为全部 9 个实验的编译运行示例：

```bash
# 实验1：环境搭建（openEuler 虚拟机 + 自动化检查脚本）
bash src/实验一/check_env.sh                   # 检查内核版本、开发工具链、网络配置

# 实验2：内核模块（电力设备驱动 /dev/power_device）
cd src/实验二/solution/cp3_device
make && sudo insmod power_device_cp3.ko
cat /dev/power_device                          # 读取电压/电流/功率
echo "380 15" | sudo tee /dev/power_device     # 下发遥控指令

# 实验3：进程通信（Pipe / 消息队列 / 共享内存 三种 IPC）
cd src/实验三/solution
make
./producer_consumer_answer                     # 生产者-消费者（电力数据采集）
./send_answer &  ./receive_answer              # 消息队列通信

# 实验4：调度平台（FCFS / SJF / RR / CFS 四算法对比）
cd src/实验四/solution/scheduler_lab
make && ./simulator                            # 运行后自动生成 *_results.csv
python3 tools/plot.py                          # 绘制 4 算法对比柱状图

# 实验5：内存管理（kmalloc / vmalloc / 大页性能对比）
cd src/实验五/solution
make && sudo insmod kmalloc_test_answer.ko     # 内核内存分配测试
sudo dmesg | tail -20                          # 查看分配结果

# 实验6：设备管理（字符设备驱动 + 电力传感器扩展）
cd src/实验六/solution
make && sudo insmod char_device.ko
ls /dev/ | grep char                           # 验证设备节点
echo "voltage" | sudo tee /dev/char_device     # 切换传感器通道

# 实验7：文件系统（用户态模拟，无需内核环境）
cd src/实验七/solution
make && ./fs_test disk.img format              # 格式化虚拟磁盘
./fs_test disk.img create power.log            # 创建电力日志文件
./fs_test disk.img write power.log "220V 10A"  # 写入
./fs_test disk.img read power.log              # 读回
./fs_test disk.img ls                          # 列目录

# 实验8：网络管理 eBPF/XDP（电力协议 UDP 40000 端口过滤）
cd src/实验八/solution/phase1_socket
make && ./udp_server &  ./udp_client           # Phase1：Socket 回显
cd ../phase2_netfilter && make && sudo insmod netfilter_hook.ko   # Phase2：Netfilter 拦截
cd ../phase3_xdp && make && sudo ip link set dev eth0 xdp obj xdp_filter.o  # Phase3：XDP

# 实验9：容器调优（iSulad 容器 + A-Tune 智能调优）
cd src/实验九/solution
sudo isula run -it openeuler/systemd bash      # 启动容器化电力应用
sudo atune-adm analysis                        # A-Tune 性能分析
bash compare_atune_answer.sh                   # 调优前后性能对比
```

各实验详细编译运行步骤请参考 `doc/` 下对应的实验指导书。

---

## 九、许可证

本项目采用 **GPL-2.0 许可证**。

详见 [非本队来源说明.md](./非本队来源说明.md)

---

**项目仓库**：https://gitlab.eduxiji.net/T2026102569910321/project3136859-386827

**参考项目**：
- OS代码小队仓库：https://gitlab.eduxiji.net/T202410256994641/project2608132-275972
- 鼠鼠仓库：https://gitlab.eduxiji.net/educg-group-39358-3035746/T202510256997804-2206

**最后更新**：2026年8月16日