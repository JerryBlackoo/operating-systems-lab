# 实验七：文件管理

用户态文件系统模拟器，实现虚拟磁盘、inode 管理、目录操作等核心功能。

## 文档索引

| 文档 | 说明 |
|------|------|
| [实验指导书.md](./实验指导书.md) | 完整实验指导、数据结构、实现步骤 |
| [设计方案.md](./设计方案.md) | 技术设计思路、磁盘布局、核心代码说明 |
| [测试报告.md](./测试报告.md) | 功能测试、性能测试、对比分析 |

## 快速开始

```bash
cd src/实验七/solution
make
./fs_test disk.img format    # 格式化
./fs_test disk.img           # 启动交互
```

## 交互命令

`format`, `mount`, `create`, `open`, `close`, `read`, `write`, `delete`, `ls`, `help`, `exit`

## 文件结构

```
src/实验七/
├── starter/          # 学生填空版
├── solution/         # 参考答案
└── tests/            # 测试脚本
```