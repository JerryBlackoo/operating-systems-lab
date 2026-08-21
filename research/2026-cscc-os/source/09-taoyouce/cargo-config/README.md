# Cargo 构建配置

比赛平台 clone 项目时会过滤隐藏目录，因此不能把构建关键配置只放在 `.cargo/` 中。

`Makefile` 的 `setup-cargo-config` 目标会在每次构建前：

1. 创建 `.cargo/`；
2. 将 `cargo-config/config.toml` 复制为 `.cargo/config.toml`；
3. 让 Cargo 使用仓库内 `vendor/` 源码目录，避免评测时在线下载依赖。

`vendor/` 是评测可复现构建的一部分，应随源码提交。
