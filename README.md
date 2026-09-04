# rbox

一个用 Rust 编写的 BusyBox 风格多合一（multi-call）二进制，交叉编译为 ARM64 (aarch64)，
运行在 QEMU 全系统模拟中。包含一个 systemd 风格的 init（PID 1，TOML 配置）、
一个支持管道/重定向/历史/Tab 补全的交互式 shell，以及 32 个常用命令。

## 特性

- **Multi-call binary**：单一二进制通过 `argv[0]` 或 `rbox <applet>` 分发 32 个命令
- **systemd 风格 init**：TOML 单元文件、依赖拓扑排序、`Type=simple/forking`、
  `Restart=on-failure/always`（退避 + 次数上限）、`Environment=`、`LogFile=`、`User=/Group=` 降权
- **服务管理**：`rservice` 命令支持 `list/status/start/stop/restart/reload`
- **有序关机/重启**：ExecStop 逆序执行、进程组清理、孤儿进程收割、kmsg 日志
- **系统初始化**：`/etc/fstab` 挂载、hostname、sysctl、PATH
- **终端登录**：`rgetty` 登录提示（常驻 fork/wait，失败/超时原地重试，`-L`/`-t` 选项，TTY 直接作为 rgetty 参数写在 ExecStart 完整命令中，`/etc/issue` 横幅）+ `rlogin` 密码校验（/etc/passwd + /etc/shadow、crypt 哈希、降权、MOTD）
- **全局配置**：`/etc/rbox.conf`（TOML）集中管理路径/提示/超时/缺省 shell 等，全部可覆盖
- **交互式 shell**：
  - 管道 `|`、重定向 `>` `>>` `<`、后台 `&`
  - 控制操作符 `;` `&&` `||`
  - 环境变量 `export VAR=val`、变量展开 `$VAR` `${VAR}` `$?` `$$`
  - 命令历史（上下键浏览）、`!!` `!n` `!-n` 历史展开
  - 行编辑：左右键移动光标、Ctrl-A/E/U/W/L、Home/End
  - Tab 补全：命令补全 + 文件/路径补全（管道后也支持命令补全）
  - 通配符 `*` `?` `[...]`、引号 `'...'` `"..."`、注释 `#`、续行 `\`
  - 内置命令：`cd` `exit` `export` `unset` `pwd` `history`
- **工程化**：Clippy 零警告、414 个单元测试、115 个集成断言、rustfmt、make strip

## 快速开始

```bash
make all       # 交叉编译 + rootfs + initramfs
make run       # QEMU 全系统模拟启动
make test      # 集成测试（QEMU 自动化验证）
make unittest  # 宿主机单元测试
make verify    # check + clippy + fmt + unittest 一键验证
```

依赖：Rust 工具链（`rustup target add aarch64-unknown-linux-gnu`）、
`gcc-aarch64-linux-gnu`、`qemu-system-arm`、Linux 内核源码（`make kernel`）。

## 目录结构

```
src/applets/
├── core/     # 系统核心：init（PID 1）及内部模块、shell/、rgetty、rlogin、shutdown、reboot、status、rservice
├── file/     # 文件操作：ls、cp、mv、rm、mkdir、touch、ln、cat
├── text/     # 文本处理：head、tail、wc、grep、printf、echo、basename、dirname
└── sys/      # 系统工具：true、false、pwd、uname、date、sleep、env
```

详细设计见 [DESIGN.md](DESIGN.md)。

## License

[MIT](LICENSE)
