# 实验二：内核模块编程

电力设备驱动程序入门，通过三个递进 Checkpoint 掌握 Linux 内核模块开发。

## 文档索引

| 文档 | 说明 |
|------|------|
| [实验指导书.md](./实验指导书.md) | 完整实验指导、Checkpoint 详情、操作步骤 |
| [设计方案.md](./设计方案.md) | 技术设计思路、数据结构、核心代码说明 |
| [测试报告.md](./测试报告.md) | 功能测试、性能测试、对比分析 |
| [电力场景.md](./电力场景.md) | 电力行业背景与扩展思考 |

## 快速开始

```bash
# Checkpoint 1: 基础模块
cd starter/cp1_basics && make && sudo insmod power_device_cp1.ko

# Checkpoint 2: 模块参数
cd ../cp2_params && make && sudo insmod power_device_cp2.ko dev_name="grid_sensor"

# Checkpoint 3: 字符设备驱动
cd ../cp3_device && make && sudo insmod power_device_cp3.ko
cat /dev/power_device
echo "380 15" | sudo tee /dev/power_device
```

## 文件结构

```
src/实验二/
├── starter/          # 学生填空版
├── solution/         # 参考答案
└── tests/            # 测试脚本
```