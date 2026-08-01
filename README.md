# rbox

一个用 Rust 编写的 BusyBox 风格多合一（multi-call）二进制，交叉编译为 ARM64 (aarch64)，
运行在 QEMU 全系统模拟中。包含一个 systemd 风格的 init（PID 1，TOML 配置）、
一个支持管道/重定向的极简 shell，以及 29 个常用命令。

## 特性

- **Multi-call binary**：单一二进制通过 `argv[0]` 或 `rbox <applet>` 分发 29 个命令
- **systemd 风格 init**：TOML 单元文件、依赖拓扑排序、`Type=simple/forking`、
  `Restart=on-failure`（退避 + 次数上限）、`Environment=`、`LogFile=`、`User=/Group=` 降权
- **服务管理**：`rservice` 命令支持 `list/status/start/stop/restart/reload`
- **有序关机/重启**：ExecStop 逆序执行、进程组清理、孤儿进程收割、kmsg 日志
- **系统初始化**：`/etc/fstab` 挂载、hostname、sysctl、PATH
- **极简 shell**：管道、输入/输出重定向、双引号、转义

## 快速开始

```bash
make all       # 交叉编译 + rootfs + initramfs
make run       # QEMU 全系统模拟启动
make test      # 集成测试（QEMU 自动化验证）
make unittest  # 宿主机单元测试
```

依赖：Rust 工具链（`rustup target add aarch64-unknown-linux-gnu`）、
`gcc-aarch64-linux-gnu`、`qemu-system-arm`、Linux 内核源码（`make kernel`）。

## 目录结构

```
src/applets/
├── core/     # 系统核心：init（PID 1）及内部模块、shell、shutdown、reboot、status、rservice
├── file/     # 文件操作：ls、cp、mv、rm、mkdir、touch、ln、cat
├── text/     # 文本处理：head、tail、wc、grep、printf、echo、basename、dirname
└── sys/      # 系统工具：true、false、pwd、uname、date、sleep、env
```

详细设计见 [DESIGN.md](DESIGN.md)。

## License

[MIT](LICENSE)
