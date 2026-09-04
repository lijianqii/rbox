# AGENTS.md — rbox

BusyBox 风格 multi-call 二进制（Rust, edition 2024），交叉编译为 aarch64-unknown-linux-gnu，
在 QEMU 全系统模拟中作为 init（PID 1）+ 交互式 shell + 32 个 applet 运行。
依赖仅 serde(derive) + toml + libc。设计文档见 DESIGN.md。

## Commands

注意：`.cargo/config.toml` 把默认 target 设为 aarch64，宿主机上跑 Rust 命令必须显式
加 `--target x86_64-unknown-linux-gnu`。

- `make verify` — CI 一键：`cargo check` + `cargo clippy -- -D warnings` + `cargo fmt --check` + `make unittest`
- `make unittest` — 宿主机单元测试：`cargo test --target x86_64-unknown-linux-gnu`
- `make all` — 交叉编译(release) + rootfs + initramfs.cpio.gz（`make build`/`rootfs`/`initramfs` 分步）
- `make run` — QEMU 启动（需先 `make kernel`；退出 Ctrl-A X）
- `make test` — QEMU 集成测试：`bash tests/run_tests.sh`（需内核 Image + 交叉工具链 + qemu）
- `make rootfs-test` — 注入 tests/units/*.toml 打包测试 initramfs，不污染生产 rootfs
- `make kernel` — 从源码交叉编译 Linux 6.12.36（kernel/ 目录，aarch64-linux-gnu- 前缀）
- `make strip` / `make clean` / `make help`

## Architecture

- `src/main.rs` — 入口与分发：argv[0] basename 分发（symlink）或 `rbox <applet>` 子命令；
  内置 `--list/--help/--version` 元命令
- `src/config.rs` — 全局配置（/etc/rbox.conf TOML：路径/提示/超时/缺省 shell，可选字段）
- `src/applet.rs` — `Applet` trait（name/help/run -> ExitCode）+ `declare_applets!` 注册表
- `src/applets/core/` — 系统核心：`init/`（PID 1，TOML 单元、依赖拓扑、Type/Restart、降权）、
  `shell/`（tokenizer/parser/expander/executor/reader/completion/builtin）、
  rgetty.rs（rgetty 登录提示，常驻 fork/wait 原地重试）、rlogin.rs（rlogin 密码校验/降权）、shutdown、reboot、status、rservice
- `src/applets/file/` — ls、cp、mv、rm、mkdir、touch、ln、cat
- `src/applets/text/` — head、tail、wc、grep、printf、echo、basename、dirname
- `src/applets/sys/` — true、false、pwd、uname、date、sleep、env
- `rootfs/` — initramfs 内容（bin/<applet> 符号链接指向 rbox，init -> bin/rbox）；init 服务单元在 `/etc/rbox/system/*.toml`
- `tests/` — run_tests.sh（QEMU 自动化断言）+ units/*.toml（测试服务单元）
- `kernel/` — Linux 源码（不进 git，需 `make kernel` 编译）

## Conventions

- 新增 applet：在对应分组（core/file/text/sys）加模块，实现 `Applet`，并在
  `src/applet.rs` 的 `declare_applets!` 加一行注册（Makefile 的 applet 列表由 `rbox --list` 自动提取）
- 注释、文档、错误消息用中文；标识符/路径/命令保持英文
- Clippy 必须零警告（`-D warnings`）；格式用 rustfmt（rustfmt.toml），`make verify` 会执行 `cargo fmt --check`
- applet 以 `ExitCode` 返回退出码；解析 `&[String]` 参数
- 宿主机单元测试（`#[cfg(test)]` 或 tests/）须在 x86_64 上跑，勿依赖 ARM64 交叉环境
- 新增 applet/功能时：同时补 `#[cfg(test)]` 单元测试与 `tests/run_tests.sh` 断言
  （集成测试当前 119 断言，覆盖 32/32 applet，汇总行必须 0 失败；文档中测试数字
  见 README 与 DESIGN.md「测试」章节，改动后同步更新）
- 集成测试通过 QEMU 驱动（tests/run_tests.sh），改动 shell/init 行为后须跑 `make test`
- init 单元文件是 TOML：`[Unit]` / `[Service]` / `[Install]`（见 tests/units/*.toml 范例）

## Notes

（后续补充）
