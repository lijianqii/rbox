# Contributing

## 环境

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu qemu-system-arm
make doctor     # 环境自检
```

## 开发流程

1. 修改代码，新增 applet 时按 AGENTS.md 的约定注册并补测试
2. `make verify` — check + clippy(`--all-targets -D warnings`) + fmt + 单元测试
3. 涉及 shell/init 行为时跑 `make test`（QEMU 集成测试）
4. 文档数字（applet 数、测试数）同步更新 README/DESIGN/AGENTS

## 约定

- 注释、文档、错误消息用中文；标识符/路径/命令用英文
- 依赖保持最小（serde/toml/libc），新依赖需说明理由
- 所有 applet 返回 `ExitCode`，解析 `&[String]` 参数，并带 `#[cfg(test)]` 单测
- 提交前确保 `make verify-all` 全绿（无内核环境至少 `make verify`）
