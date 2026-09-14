# rbox

一个用 Rust 编写的 BusyBox 风格多合一（multi-call）二进制，交叉编译为 ARM64 (aarch64)，
运行在 QEMU 全系统模拟中。包含一个 systemd 风格的 init（PID 1，TOML 配置）、
一个支持管道/重定向/历史/Tab 补全/复合命令/作业控制/脚本编程原语的交互式 shell，以及 65 个常用命令。

## 特性

- **Multi-call binary**：单一二进制通过 `argv[0]` 或 `rbox <applet>` 分发 65 个命令
- **systemd 风格 init**：TOML 单元文件、依赖拓扑排序、`Type=simple/forking`、
  `Restart=on-failure/always`（固定 RestartSec 间隔 + 次数上限）、`Environment=`、`LogFile=`、`User=/Group=` 降权
- **服务管理**：`rservice` 命令支持 `list/status/start/stop/restart/reload`
- **有序关机/重启**：ExecStop 逆序执行、进程组清理、孤儿进程收割、kmsg 日志
- **系统初始化**：`/etc/fstab` 挂载、hostname、sysctl、PATH
- **终端登录**：`rgetty` 登录提示（常驻 fork/wait，失败/超时原地重试，`-L`/`-t` 选项，TTY 直接作为 rgetty 参数写在 ExecStart 完整命令中，`/etc/issue` 横幅）+ `rlogin` 密码校验（/etc/passwd + /etc/shadow、crypt 哈希、降权、MOTD）
- **全局配置**：`/etc/rbox.conf`（TOML）集中管理路径/提示/超时/缺省 shell 等，全部可覆盖
- **持久化 rootfs**：`make disk` 生成 ext4 镜像，init 支持 `root=` 内核参数 switch_root（脱离 initramfs）
- **交互式 shell（可作为 /bin/sh 运行脚本）**：
  - **脚本模式**：`sh script.sh args...`、`sh -c 'cmd' name args...`、`#!` 脚本、
    stdin 脚本；选项 `-e`（errexit）/`-x`（xtrace）/`-u`（nounset）/`-o pipefail`
  - **函数**：`f() { ... }` / `function f { ... }`，`local`、`return N`；`case`/`until`、
    `break N`/`continue N`
  - 管道 `|`、`|&`、重定向 `>` `>>` `<` `2>` `2>&1` `>&N` `&>` `<<<` `>&-`（`set -C` 防覆盖）
  - 控制操作符 `;` `&&` `||`、后台 `&`
  - 展开：`$VAR` `"$VAR"`（词分割语义）、`$?` `$$` `$!` `$#` `$1..$9` `"$@"`、
    `${VAR:-def}` `${VAR:=def}` `${VAR:?err}` `${VAR:+alt}` `${#VAR}`、
    `${VAR#pat}` `${VAR##pat}` `${VAR%pat}` `${VAR%%pat}` `${VAR/old/new}`、
    `$(( ))`（含赋值/比较/逻辑）、`$( )`、`$(<file)`、`$'\n'`、`{a,b}` `{1..5}`
  - 变量赋值 `VAR=val`、`VAR=val cmd`；命令历史（上下键、`!!` `!n` `!-n` `!$`、`history [-c|N]`、
    `HISTFILE`/`HISTSIZE`）
  - 行编辑：左右键移动光标、Ctrl-A/E/U/W/L、Home/End；Tab 补全（命令 + 文件/路径）
  - 通配符 `*` `?` `[...]`（引号内不展开）、引号 `'...'` `"..."`、注释 `#`、续行 `\`、
    here-doc（支持变量/命令展开、`<<-`、`<<'EOF'`、脚本模式）
  - 别名 `alias`/`unalias`；`source`/`.`；`eval`；`command -v/-V`；`type`；`umask`；`let`；`times`
  - 作业控制：`jobs [-l]` / `fg` / `bg` / `wait [pid|%job]` / `disown`，
    `%+`/`%-`/`%?str` 作业规格，`kill %1`，Ctrl-Z 挂起/恢复，终端前台进程组交接
  - 信号：`trap 'cmd' EXIT/INT/TERM/...`；`exec`（替换进程 + 永久重定向）
  - 内置命令：`cd`（含 `cd -`、`PWD`/`OLDPWD`）`exit` `export` `unset` `pwd` `history`
    `alias` `unalias` `jobs` `fg` `bg` `read`（`-r -s -t -n -d -p`）`set` `shift`
    `exec` `wait` `disown` `return` `trap` `type` `hash` `umask` `let` `times` `local`
    `break` `continue`
  - `PS2` 续行提示、`PS4` xtrace 前缀、`IFS` 词分割可配置；启动时 source `/etc/profile`
    与 `~/.profile`
- **工程化**：Clippy（--all-targets）零警告、791 个单元测试、247 个集成断言、rustfmt、fuzz-lite 随机化测试、musl 静态构建、make doctor 环境自检

## 快速开始

```bash
make all       # 交叉编译 + rootfs + initramfs
make run       # QEMU 全系统模拟启动
make test      # 集成测试（QEMU 自动化验证）
make unittest  # 宿主机单元测试
make verify    # check + clippy + fmt + unittest 一键验证
make verify-all # verify + QEMU 集成测试
make build-musl # musl 静态构建（无 glibc 运行时依赖）
make coverage  # 覆盖率（需 cargo-llvm-cov）
make dist      # 发布包 dist/rbox-VERSION-aarch64.tar.gz
```

依赖：Rust 工具链（`rustup target add aarch64-unknown-linux-gnu`）、
`gcc-aarch64-linux-gnu`、`qemu-system-arm`、Linux 内核源码（`make kernel`）。

## 目录结构

```
src/applets/
├── core/     # 系统核心：init（PID 1）及内部模块、shell/、rgetty、rlogin、shutdown、reboot、status、rservice
├── file/     # 文件操作：ls、cp、mv、rm、mkdir、touch、ln、cat、chmod、chown、find、stat、du、df、readlink、realpath、mktemp、sync、dd、tar
├── text/     # 文本处理：head、tail、wc、grep、printf、echo、basename、dirname、sort、uniq、cut、tr、tee
└── sys/      # 系统工具：true、false、pwd、uname、date、sleep、env、kill、dmesg、mount、umount、meminfo、processes、logkeeper、test、id、hostname、uptime、timeout、pgrep、pkill、passwd、su
```

详细设计见 [DESIGN.md](DESIGN.md)。

## License

[MIT](LICENSE)
