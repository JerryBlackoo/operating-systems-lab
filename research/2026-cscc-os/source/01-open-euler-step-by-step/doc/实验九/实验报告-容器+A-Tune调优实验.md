# 实验报告：iSulad 轻量级容器 + A-Tune 智能调优实验

## 一、实验基本信息

| 项目 | 内容 |

|------|------|

| 项目编号 | proj54 |

| 实验名称 | OpenEuler 内核 Step by Step - 容器与智能调优 |

| 实验环境 | VirtualBox + openEuler 24.03 LTS (SP3) |

| 实验日期 | 2026年6月30日 |

## 二、实验目的

1. 掌握 iSulad 轻量级容器引擎的安装与基本使用

2. 掌握 A-Tune 智能系统性能调优框架的安装与配置

3. 通过基准测试验证 A-Tune 在不同场景下的调优效果

4. 探索 A-Tune 在虚拟化容器环境中的适用性

## 三、实验环境

### 3.1 硬件配置

| 项目 | 配置 |

|------|------|

| CPU | 2 核 (VirtualBox 虚拟化) |

| 内存 | 4 GB |

| 磁盘 | 20 GB |

| 网络 | 桥接模式 |

### 3.2 软件版本

| 软件 | 版本 |

|------|------|

| 操作系统 | openEuler 24.03 LTS (SP3) |

| 内核版本 | 6.6.0-132.0.0.111.oe2403sp3.x86\_64 |

| iSulad | 最新版 (通过 dnf 安装) |

| isula-build | 0.9.6-24.oe2403sp3.x86\_64 |

| A-Tune | 最新版 (通过 dnf 安装) |

## 四、实验步骤与结果

### 4.1 iSulad 安装与配置

#### 4.1.1 安装 iSulad

```bash

dnf install -y iSulad isula-build

4.1.2 配置镜像加速

创建 /etc/isulad/daemon.json 配置文件，添加国内镜像加速源：

json

{"registry-mirrors": ["https://docker.m.daocloud.io","https://dockerproxy.com"]}

4.1.3 启动 iSulad 服务

bashsystemctl enable --now isuladsystemctl status isulad

4.1.4 拉取镜像并运行容器bash

拉取 openEuler 基础镜像

risula pull hub.oepkgs.net/openeuler/openeuler:24.03

运行容器验证

risula run --rm hub.oepkgs.net/openeuler/openeuler:24.03 cat /etc/os-release

4.2 A-Tune 安装与配置

4.2.1 安装 A-Tune

bash

dnf install -y atune atune-engine atune-collector

4.2.2 启动 A-Tune 服务

bash

systemctl enable --now atuned

systemctl status atuned

4.2.3 查看可用调优模板

bash

atune-adm list

可用模板列表：

| 模板名称 | 用途 | 适用场景 |
| --- | --- | --- |
| basic-test-suite-baseline-fio | 存储IO优化 | 磁盘密集型 |
| basic-test-suite-baseline-lmbench | 综合性能优化 | 通用场景 |
| basic-test-suite-baseline-netperf | 网络性能优化 | 网络密集型 |
| basic-test-suite-baseline-stream | 内存带宽优化 | 科学计算/AI推理 |
| basic-test-suite-baseline-unixbench | 通用性能优化 | 基准测试 |

4.2.4 激活调优模板

atune-adm profile basic-test-suite-baseline-stream

验证激活状态：

bash

atune-adm list | grep true

# 输出: basic-test-suite-baseline-stream | true

4.3 性能基准测试

4.3.1 CPU 计算性能测试（圆周率计算）

测试命令：

bash

time echo "scale=5000; 4\*a(1)" | bc -l

| 测试状态 | 耗时 (real) |
| --- | --- |
| A-Tune 关闭 | 11.771s |
| A-Tune 开启 (netperf) | 11.743s |
| A-Tune 开启 (stream) | 11.965s |

结论： A-Tune 的调优对纯 CPU 计算任务影响不显著，测试结果在正常波动范围内。

4.3.2 内存带宽测试（dd 写入测试）

测试命令：

bash

dd if=/dev/zero of=/tmp/test bs=1M count=1024 conv=fdatasync

测试结果：

| 测试状态 | 写入速度 | 备注 |
| --- | --- | --- |
| A-Tune 开启 (stream) | 1.9 GB/s | 性能下降约 60% |
| A-Tune 关闭 | 4.7 GB/s | 基准性能 |
| A-Tune 关闭（重复） | 4.4 GB/s | 验证一致性 |

结论： 在虚拟机环境中，basic-test-suite-baseline-stream 模板产生了负优化效果，内存写入性能显著下降。分析认为，该模板针对物理机硬件设计，虚拟化层的内存访问机制与物理机存在差异，导致某些优化参数在虚拟化环境下产生副作用。

4.4 实验结果分析

4.4.1 关键发现

1、iSulad 安装配置顺利完成：轻量级容器引擎在 openEuler 上运行稳定，资源占用低（约 33MB 内存）。

2、A-Tune 在虚拟化环境中的局限性：

物理机优化的 profile 在虚拟化环境中可能产生负优化

调优前必须进行基准测试验证

3、测试工具局限性：

圆周率计算（纯 CPU）无法充分体现 A-Tune 的优势

内存带宽测试（dd）对 I/O 调度敏感，虚拟机环境干扰较大

4.4.2 经验总结

1、环境适配很重要：A-Tune 的 profile 需要根据实际硬件环境选择，不能盲目应用默认模板。

2、基准测试是前提：调优前后必须进行充分的基准测试，确保调优确实带来正向收益。

3、虚拟化环境需谨慎：在虚拟机中使用系统调优工具时，需要特别注意虚拟化层引入的性能影响。

五、实验总结

本次实验完成了 iSulad 容器引擎和 A-Tune 智能调优框架的安装配置，并通过基准测试分析了 A-Tune 在虚拟化环境中的调优效果。

实验结果表明：

iSulad 安装简便，运行稳定，资源占用低，适合容器化部署场景。

A-Tune 的调优效果与硬件环境强相关，在虚拟化环境中某些 profile 可能产生负优化，需要针对具体场景进行验证。

后续工作：

在容器环境中运行实际的 AI 推理任务，进一步验证 A-Tune 在应用层面的调优效果

探索 A-Tune 动态调优（tuning）功能，实现更智能的参数优化

六、参考文献

1、openEuler 官方文档 - iSulad 用户指南

2、openEuler 官方文档 - A-Tune 用户指南

3、iSula 轻量级容器技术入门.pptx（团队内部资料）