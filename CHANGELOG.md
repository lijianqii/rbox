# Changelog

本项目遵循「里程碑式」记录（无严格 SemVer 版本节奏）。

## [0.1.0] - 2026-09

### 核心
- BusyBox 风格 multi-call 二进制：argv[0] symlink 分发 + `rbox <applet>` 子命令
- systemd 风格 init（PID 1）：TOML 单元、依赖拓扑（Requires/After/Wants/Requisite/Before）、
  分层并发启动、Type=simple/forking、Restart=on-failure/always + 退避/次数上限、
  Environment=/EnvironmentFile=/WorkingDirectory=/LogFile=/User=/Group=、
  TimeoutStartSec/TimeoutStopSec/KillMode、ExecStop/ExecReload、
  unix socket 控制协议（status/rservice）、硬件看门狗喂狗、SIGCHLD 事件驱动回收、
  ext4 持久根 switch_root（root= 支持裸设备与 UUID=/PARTUUID=/LABEL=）、
  启动失败降级 rescue shell
- 交互式 shell：管道/重定向（含 `2>&1`）/`;`/`&&`/`||`/`&`、变量与位置参数
  （`$VAR` `${VAR}` `$?` `$$` `$#` `$1..$9` `$@`）、算术 `$(( ))`、命令替换 `$()`、
  别名、`if/elif/else`、`for`、`while`（含 `break`/`continue`）、历史与 `!!`/`!n`/`!$`、
  Tab 补全、行编辑、作业控制（`jobs`/`fg`/`bg` + Ctrl-Z）、here-doc、`source`、PS1 展开
- 登录：rgetty（常驻 fork/wait、`-L`/`-t` 超时）+ rlogin（shadow + crypt、降权、MOTD）、
  `su`/`passwd`
- 65 个 applet：文件/文本/进程/系统工具（含 tar/dd/sort/cut/tr/test/find/mount 等）

### Shell 脚本化（v0.1.0 内后续迭代）
- 脚本模式：`sh script.sh args`、`sh -c`、shebang、stdin 脚本；`-e/-x/-u/-o pipefail`
- 函数（`local`/`return`）、`case`、`until`、`break N`/`continue N`
- 参数展开运算符、词分割与 `"$@"`、`$'...'`、花括号展开、算术赋值/比较/逻辑、`$(<file)`
- 重定向补齐：`&>`/`&>>`/`|&`/`<<<`/`>&-`/`N<&M`、here-doc 展开与 `<<-`、`set -C`
- 内置：`exec`/`wait`/`trap`/`eval`/`command`/`type`/`umask`/`let`/`times`/`local`/`disown`，
  `read` 选项（`-r -s -t -n -d -p`）、`history -c`/`N`、`HISTFILE`/`HISTSIZE`
- 作业控制：终端前台进程组交接（tcsetpgrp）、`%+`/`%-`/`%?str` 规格、`$!`
- 交互配置：`PS2`/`PS4`/`IFS`、`~/.profile`、`cd -`/`PWD`/`OLDPWD`

### 工程
- 单测 765 个、QEMU 集成断言 200 条（含登录/超时、rescue、持久盘、emergency/single）
- Clippy `--all-targets -D warnings` 零告警、rustfmt、make verify / verify-all
- release profile（thin LTO + strip）、musl 静态构建（rust-lld）、fuzz-lite 随机化测试、
  coverage/audit/dist 目标
