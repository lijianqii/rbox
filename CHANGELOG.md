# Changelog

本项目遵循「里程碑式」记录（无严格 SemVer 版本节奏）。

## [未发布] - 2026-09

### Shell：对齐 BusyBox ash
- 子 shell `( ... )`（fork 隔离变量/cwd/重定向）、花括号组 `{ ...; }`、`!` 取反
- 反引号命令替换；`$()`/`$(( ))`/反引号内部 `;`/`&`/`|` 不再被误分词
- `:`、readonly（赋值/unset 保护）、getopts（OPTIND/OPTARG）、ulimit（-a/-H/-S，rlimit）
- `kill` 内置支持 `%job` 规格；`$-` 选项串；`set -o`/`set +o` 名称列表与重置
- 算术扩展：位运算 `& | ^ ~`、三元 `?:`、逗号、前置/后缀 `++ --`
- 参数子串 `${var:offset[:length]}`（含负偏移）
- 重定向：`<>`、`>|`（set -C 强制覆盖）、任意 `N>`/`N>>`/`N<`、`N<&M`/`N>&-`、
  复合命令尾重定向（`while ...; done < file`，循环内变量持久）、子 shell/组重定向
- 复合命令可出现在 `;` 之后（`set -- p q; for x; do ...; done`）
- cd 支持 CDPATH、`-P`/`-L`；pwd 接受 `-P`/`-L`；`$ENV` 启动文件；`set -o ignoreeof`
- `set -f`（noglob）、`$RANDOM`、行内 `!` 取反仅作用于首个 pipeline
- 未覆盖项补齐：算术 `**` 幂运算（右结合，一元负号作用于整个幂）、`cd -L` 逻辑路径
  （默认，保留符号链接与 `..` 文本语义）与 `cd -P`/`pwd -P` 物理路径、`kill -l` 无参
  4 列表格、`cmd && ( ... )`/`cmd || { ...; }` 条件组、`$ENV` 启动文件与 `set -o ignoreeof`
  的端到端验证
- BusyBox ash 1:1 对齐（以 busybox ash 为基准实测）：`set -o` 输出 `name on|off`、
  `export -p`/`readonly -p` 引号格式、`jobs -p`、`set -b` 反映到 `$-`、`kill -l`
  逐行 `N) NAME`；删除 ash 无的 `disown` 与 `trap -l`
- 测试补齐：新增 5 个单测与 13 条集成断言（noclobber/`<>`/任意 fd/`$-`/`set -o`/
  `$RANDOM`/CDPATH/`kill %job`/命令替换多行输出/负偏移子串等），并加固重定向类测试的
  并发输出隔离；`make coverage` 报告整体约 73% 行 / 83% 函数覆盖

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
- 单测 791 个、QEMU 集成断言 247 条（含登录/超时、rescue、持久盘、emergency/single）
- Clippy `--all-targets -D warnings` 零告警、rustfmt、make verify / verify-all
- release profile（thin LTO + strip）、musl 静态构建（rust-lld）、fuzz-lite 随机化测试、
  coverage/audit/dist 目标
