# rbox

一个用 Rust 编写的 BusyBox 风格多合一（multi-call）二进制，交叉编译为 ARM64 (aarch64)，在 QEMU 全系统模拟中运行。包含一个 systemd 风格的 init（PID 1，TOML 配置）和一个支持管道/重定向的极简 shell。

> 本文档面向后续接手的 AI 或人类开发者，描述当前已实现的功能、架构、构建方式和后续计划。

## 项目概述

| 属性 | 值 |
|------|-----|
| 语言 | Rust (edition 2024) |
| 目标架构 | aarch64-unknown-linux-gnu |
| libc | glibc（动态链接） |
| 运行环境 | QEMU 全系统模拟（qemu-system-aarch64） |
| 内核 | Linux 6.12.36 LTS，本机从源码交叉编译（defconfig，ARM64） |
| 依赖 | serde + toml + libc（libc 用于 init/系统调用） |
| 二进制大小 | ~1.4MB（release + strip + LTO）；musl 静态 ~1.5MB |
| initramfs 大小 | ~1.7MB |

**设计理念**：单一二进制 rbox 通过 argv[0] basename 分发或 rbox subcommand 子命令分发，模拟 BusyBox 的 multi-call binary 模式。一个二进制既是 init、又是 shell、又是所有用户命令。
## 环境与工具链

### 本机环境

- OS: linux/amd64
- Rust: cargo/rustc 1.97.0 (edition 2024)
- 交叉链接器: aarch64-linux-gnu-gcc 14.2.0
- QEMU: qemu-system-aarch64（全系统模拟，非 user-mode）

### Rust 工具链配置

`.cargo/config.toml`：

```toml
[build]
target = "aarch64-unknown-linux-gnu"

[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
```

已安装的 Rust target：aarch64-unknown-linux-gnu、x86_64-unknown-linux-gnu

### 需要的软件包

```bash
# Rust 工具链
rustup target add aarch64-unknown-linux-gnu

# 交叉编译器（提供 glibc sysroot）
sudo apt install gcc-aarch64-linux-gnu

# QEMU
sudo apt install qemu-system-arm

# 内核编译依赖
sudo apt install libelf-dev flex bison bc cpio libssl-dev
```

### 内核编译

内核源码放在 kernel/ 目录（Linux 6.12.36 LTS），使用 defconfig。`make kernel`
为幂等目标：若 `kernel/` 源码缺失（无 Makefile），先用 `xz -t` 校验根目录已有的
`linux-6.12.36.tar.xz`，校验通过则复用，损坏/缺失则自动从**清华开源镜像站**
(`https://mirrors.tuna.tsinghua.edu.cn/kernel/v6.x/linux-6.12.36.tar.xz`)
下载并解压到 `kernel/`（下载或解压失败会自动删除损坏包以便下次重试）；若设置了
`KERNEL_SHA256` 还会做 sha256 校验。随后仅在 `.config` 缺失时生成 defconfig、
仅在 `Image` 缺失时编译。

```bash
make kernel   # 源码缺失时自动下载清华镜像并编译，已存在则跳过对应步骤
```

也可手动分步：

```bash
cd kernel
make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- defconfig
make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- -j$(nproc) Image
```

产物：kernel/arch/arm64/boot/Image（38MB，ARM64 boot executable）。
## 项目结构

```
rbox/
├── Cargo.toml              # 项目配置 + 依赖
├── .cargo/config.toml      # 交叉编译 target + linker 配置
├── Makefile                # 构建系统
├── src/
│   ├── main.rs             # 入口，argv[0]/subcommand 分发 + --list/--help/--version
│   ├── applet.rs           # Applet trait + 全局 APPLETS 注册表
│   ├── config.rs           # 全局配置（/etc/rbox.conf TOML：路径/提示/超时/缺省 shell）
│   └── applets/            # 按功能分四组（core/file/text/sys）
│       ├── mod.rs          # 子模块声明
│       ├── core/           # 系统核心 applet + init 内部实现
│       │   ├── mod.rs      # 模块声明 + 共享 log()（kmsg/console）
│       │   ├── init/       # PID 1 实现（非 applet，仅 init 使用）
│       │   │   ├── mod.rs  # 入口：run、早期根切换、拓扑分层启动、主循环
│       │   │   ├── signals.rs  # 信号处理器、self-pipe、关机/重启标志
│       │   │   ├── watchdog.rs # 硬件看门狗（打开/喂狗/poll 超时压缩）
│       │   │   ├── boot.rs     # 启动模式（single/emergency）与应急 shell
│       │   │   ├── shutdown.rs # 有序关机/重启
│       │   │   ├── units.rs    # 单元 TOML 解析、单元名、拓扑排序
│       │   │   ├── services.rs # 服务生命周期：spawn/daemon化/重启退避/停止/降权
│       │   │   ├── server.rs   # 控制协议服务端（status/start/stop/restart/reload）
│       │   │   ├── mount.rs    # fstab 挂载、hostname、sysctl
│       │   │   └── syscall.rs  # libc 系统调用封装
│       │   ├── control.rs  # 控制协议客户端（status/rservice 共用）
│       │   ├── shell/       # 命令解释器（mod/tokenizer/parser/expander/completion/builtin/executor/reader/types/alias/compound/jobs）
│       │   ├── rgetty.rs    # rgetty（终端登录提示，常驻 fork/wait 原地重试）
│       │   ├── rlogin.rs    # rlogin（密码校验、降权、exec 用户 shell）
│       │   ├── shutdown.rs # shutdown（向 PID 1 发 SIGTERM）
│       │   ├── reboot.rs   # reboot（向 PID 1 发 SIGINT）
│       │   ├── status.rs   # status [unit]（unix socket 查询 init 服务状态）
│       │   └── rservice.rs # rservice（unix socket 管理 init 服务：start/stop/restart/reload）
│       ├── file/           # 文件操作：ls、cp、mv、rm、mkdir、touch、ln、cat、chmod、chown、find（+ util.rs）
│       ├── text/           # 文本处理：head、tail、wc、grep、printf、echo、basename、dirname（+ util.rs）
│       ├── sys/            # 系统工具：true、false、pwd、uname、date、sleep、env、meminfo、processes、logkeeper、kill、dmesg、mount、umount
│       ├── proc.rs         # 共享工具：进程收集/解析（ProcMem）、human_size 单位格式化
│       ├── glob.rs         # 共享工具：glob 匹配（shell 通配符与 find -name 共用）
│       └── fstab.rs        # 共享工具：/etc/fstab 解析（init 挂载与 mount 命令共用）
├── rootfs/                 # 根文件系统目录树
│   ├── init -> bin/rbox    # init 符号链接
│   ├── bin/
│   │   ├── rbox            # 主二进制（ARM64 ELF）
│   │   └── (各 applet -> rbox 符号链接)
│   ├── lib/                # glibc 运行时
│   └── etc/
│       ├── hostname
│       └── rbox/system/    # init TOML 单元文件
├── initramfs.cpio.gz       # 打包好的 initramfs
├── kernel/                 # Linux 内核源码 + 编译产物
└── tests/
    ├── run_tests.sh        # 集成测试脚本（注入 tests/units 测试服务）
    ├── login-console.service.toml  # 登录超时测试专用 console 单元（-t 8）
    ├── rbox.test.conf      # 登录超时测试专用全局配置（password_timeout=3）
    └── units/              # 测试专用服务单元（运行时注入 rootfs，不入生产镜像）
```
## 架构设计

### Multi-call Binary 分发

```
用户输入: $ echo hello
           │
           ▼
    argv[0] = "/bin/echo"
    basename = "echo"
           │
           ▼
    main.rs 分发逻辑:
    ┌─ basename == "rbox" -> subcommand 模式
    │   rbox echo hello -> argv[1]="echo" 是命令, argv[2..] 是参数
    │
    └─ basename != "rbox" -> argv[0] 分发模式
        basename = "echo" -> 查 APPLETS 表 -> 执行 Echo applet
```

**双分发模式**：

1. **subcommand 模式**：`rbox <applet> [args...]` - argv[1] 是 applet 名，argv[2..] 是参数
2. **argv[0] 模式**：通过 symlink（如 `bin/echo -> rbox`），basename 即 applet 名，argv[1..] 是参数

`sh` 是 shell 的 applet 名（`bin/sh -> rbox`），通过 symlink 直接分发，无需额外映射。

### Applet Trait

```rust
pub trait Applet: Sync {
    fn name(&self) -> &'static str;      // 命令名，如 "echo"
    fn help(&self) -> &'static str { "" } // 简短帮助
    fn run(&self, args: &[String]) -> ExitCode;  // 执行
}
```

每个 applet 文件定义一个 `pub static XXX: &Yyy = &Yyy;`，注册到 `applet.rs` 的 `APPLETS` 数组。

### 新增 Applet 步骤

1. 创建 `src/applets/<category>/<name>.rs`（按功能选 core/file/text/sys），实现 Applet trait
2. 在对应 `src/applets/<category>/mod.rs` 添加 `pub mod <name>;`
3. 在 `src/applet.rs` 的 APPLETS 数组添加 `crate::applets::<category>::<name>::XXX,`
4. 在 `Makefile` 的 APPLETS 变量添加 applet 名（用于 rootfs 符号链接）

### Shell 命令查找回退

shell 在 fork+exec 时，如果 PATH 查找失败，会回退尝试 `rbox <cmd>` -- 这样即使没有为某个 applet 创建 symlink，也能通过 shell 执行内置命令。
## 已实现的 Applet

共 65 个 applet：

| # | Applet | 用法 | 说明 |
|---|--------|------|------|
| 1 | true | true | 返回退出码 0 |
| 2 | false | false | 返回退出码 1 |
| 3 | echo | echo [-n] [args...] | 打印参数，-n 不换行 |
| 4 | cat | cat [files...] | 拼接文件到 stdout，无参数读 stdin |
| 5 | pwd | pwd | 打印当前工作目录 |
| 6 | uname | uname [-asnrvm] | 打印系统信息，-m 输出 aarch64 |
| 7 | ls | ls [-a] [-l] [-1] [files...] | 列目录，-a 全部、-l 长格式、-1 每行一个 |
| 8 | cp | cp SOURCE DEST | 复制文件 |
| 9 | mv | mv SOURCE DEST | 移动/重命名文件 |
| 10 | rm | rm [-r] [-f] FILES... | 删除文件，-r 递归、-f 强制 |
| 11 | mkdir | mkdir [-p] DIRS... | 创建目录，-p 递归创建 |
| 12 | touch | touch FILES... | 创建空文件或更新时间戳 |
| 13 | init | init | PID 1 系统初始化（见下文） |
| 14 | sh | sh | 命令解释器（见下文） | |
| 15 | shutdown | shutdown | 向 PID 1 发 SIGTERM 触发有序关机 |
| 16 | reboot | reboot | 向 PID 1 发 SIGINT 触发有序重启 |
| 17 | head | head [-n N] [file] | 输出文件前 N 行（默认 10） |
| 18 | tail | tail [-n N] [file] | 输出文件后 N 行（默认 10） |
| 19 | wc | wc [-l] [-w] [-c] [file] | 统计行数/单词数/字节数 |
| 20 | grep | grep [-i] [-n] [-v] PATTERN [file] | 文本搜索 |
| 21 | ln | ln [-s] TARGET LINK | 创建链接（默认硬链接，-s 符号链接） |
| 22 | date | date | 显示当前日期时间 |
| 23 | sleep | sleep N | 睡眠 N 秒（支持小数） |
| 24 | env | env [VAR=val] [cmd] | 显示或设置环境变量 |
| 25 | printf | printf FORMAT [args] | 格式化输出（%s/%d/%x/%c） |
| 26 | basename | basename PATH [SUFFIX] | 取文件名部分 |
| 27 | dirname | dirname PATH | 取目录部分 |
| 28 | status | status [unit] | 通过 unix socket 查询 init 服务状态 |
| 29 | rservice | rservice [list\|status\|start\|stop\|restart <unit>] | 服务管理：列出/启动/停止/重启服务 |
| 30 | rgetty | rgetty [-L] [-t SEC] [TTY] | 终端登录提示，fork rlogin 常驻重试（由 init 的 Restart=always 服务拉起） |
| 31 | rlogin | rlogin [username] | 校验密码（/etc/passwd + /etc/shadow），成功后降权并 exec 用户 shell |
| 32 | meminfo | meminfo [-bkmg] [-a] | 内存总览 + 分类核算（与 MemTotal 对账）+ 明细 + iomem 树 + 进程列表 |
| 33 | processes | processes | 进程树：system 大分组，每行 PID 名称(Command) State RSS MEM% |
| 34 | logkeeper | logkeeper [FILE] | 将 /dev/kmsg 转发到日志文件（持久化，Restart=always 服务） |
| 35 | dmesg | dmesg [-n N] [-c] | 查看内核环形缓冲区（klogctl 全量读取，无权限回退 /dev/kmsg；-c 清空） |
| 36 | kill | kill [-SIGNAL] PID... / kill -l [SIG] | 向进程发送信号（名字/数字/`-l` 映射，`--` 后为 PID） |
| 37 | mount | mount [-t TYPE] [-o OPTS] [DEVICE DIR\|TARGET] | 挂载文件系统（无参列出；TARGET 查 /etc/fstab；-r/-w 简写） |
| 38 | umount | umount [-f] [-l] TARGET... | 卸载文件系统（umount2，支持强制/惰性） |
| 39 | chmod | chmod [-R] MODE FILE... | 修改权限（八进制/符号，支持 s/t/X，递归不跟随符号链接） |
| 40 | chown | chown [-R] USER[:GROUP] FILE... | 修改属主/属组（lchown，支持 `user:`/`:group`） |
| 41 | find | find [PATH...] [-name PATTERN] [-type f\|d] [-maxdepth N] | 递归查找文件（不跟随符号链接，按路径排序） |
| 42 | test | test EXPR | 条件表达式（文件/字符串/数值/逻辑，含 -a -o ! 括号） |
| 43 | [ | [ EXPR ] | test 的别名（要求末尾 ]） |
| 44 | sort | sort [-nruf] [file...] | 行排序（数值/逆序/去重/忽略大小写） |
| 45 | uniq | uniq [-cdu] [file] | 相邻重复行去重（计数/仅重复/仅唯一） |
| 46 | cut | cut -d DELIM -f LIST [-s] \| cut -c LIST | 按分隔符取字段 / 按字符位置截取 |
| 47 | tr | tr [-d] [-s] SET1 [SET2] | 字符转换/删除/压缩（支持范围与转义） |
| 48 | tee | tee [-a] [file...] | stdin 同时写 stdout 与文件 |
| 49 | stat | stat [-c FORMAT] FILE... | 文件元数据（%n %s %a %u %g %F %y 等格式码） |
| 50 | du | du [-s] [-h] [path...] | 目录/文件磁盘占用统计 |
| 51 | df | df [-h] | 文件系统使用（/proc/mounts + statfs） |
| 52 | readlink | readlink [-f] [-n] PATH... | 读符号链接目标 / 规范化 |
| 53 | realpath | realpath PATH... | 输出规范化绝对路径 |
| 54 | mktemp | mktemp [-d] [-u] [template] | 创建唯一临时文件/目录 |
| 55 | sync | sync | 刷新文件系统缓冲 |
| 56 | dd | dd [if= of= bs= count= skip= seek=] | 按块复制数据 |
| 57 | tar | tar -c\|-x\|-t [-f FILE] [-v] [-C DIR] | ustar 打包/解包（普通文件/目录/符号链接） |
| 58 | id | id [-u] [-g] [-G] [-n] [user] | 用户/组身份 |
| 59 | hostname | hostname [-s] [NAME] | 显示/设置主机名 |
| 60 | uptime | uptime | 运行时长与负载 |
| 61 | timeout | timeout [-s SIG] DURATION CMD... | 限时运行命令（超时 124） |
| 62 | pgrep | pgrep [-f] [-x] [-l] PATTERN | 按名称/命令行查找进程 |
| 63 | pkill | pkill [-f] [-x] [-SIGNAL] PATTERN | 按名称/命令行发送信号 |
| 64 | passwd | passwd [user] | 修改密码（SHA-512 crypt 写 /etc/shadow） |
| 65 | su | su [user] | 切换用户并启动 shell（shadow 校验） | |
## Shell

文件：src/applets/core/shell/（模块目录，含单元测试）

一个命令解释器，REPL 循环逐字节读取输入并执行。提示符：`> `（续行时也是 `> `）

### 功能

| 功能 | 语法 | 状态 |
|------|------|------|
| 命令执行 | cmd arg1 arg2 | 已实现 |
| 多级管道 | cmd1 \| cmd2 \| cmd3 | 已实现 |
| 输出重定向（覆盖） | cmd > file | 已实现 |
| 输出重定向（追加） | cmd >> file | 已实现 |
| 输入重定向 | cmd < file | 已实现 |
| 内置命令 cd | cd /path | 已实现 |
| 内置命令 exit | exit [code] | 已实现 |
| 内置命令 export | export VAR=value | 已实现 |
| 内置命令 unset | unset VAR | 已实现 |
| 内置命令 pwd | pwd | 已实现 |
| 双引号保留空格 | "hello world" | 已实现 |
| 单引号原样保留 | 'a b c' | 已实现 |
| 反斜杠转义 | hello\\ world | 已实现 |
| 反斜杠续行 | echo hello \\ + world | 已实现 |
| 注释 | echo hello # comment | 已实现 |
| 环境变量 $VAR | echo $VAR | 已实现 |
| 花括号变量 ${VAR} | echo ${VAR}_x | 已实现 |
| 退出码 $? | false; echo $? | 已实现 |
| PID $$ | echo $$ | 已实现 |
| 命令分隔 ; | echo a; echo b | 已实现 |
| 条件执行 && | true && echo yes | 已实现 |
| 条件执行 \|\| | false \|\| echo fb | 已实现 |
| 后台运行 & | sleep 1 & echo done | 已实现 |
| 通配符 * | ls *.txt | 已实现 |
| 通配符 ? | ls x? | 已实现 |
| 通配符 [] | ls [ab].txt | 已实现 |
| Tab 补全（命令） | ec<Tab> -> echo | 已实现 |
| Tab 补全（文件） | cat /etc/host<Tab> | 已实现 |
| 命令历史（上/下键） | <Up> 回溯上一条命令 | 已实现 |
| 光标移动（左/右键） | <Left>/<Right> 移动光标 | 已实现 |
| 行内编辑 | 在光标处插入/删除字符 | 已实现 |
| Ctrl-A / Home | 跳到行首 | 已实现 |
| Ctrl-E / End | 跳到行末 | 已实现 |
| Ctrl-C | 中断当前行，新起提示符 | 已实现 |
| Ctrl-L | 清屏并重绘当前行 | 已实现 |
| Ctrl-U | 删除光标前所有内容 | 已实现 |
| Ctrl-K | 删除光标后所有内容 | 已实现 |
| Ctrl-W | 删除光标前一个单词 | 已实现 |
| Delete 键 | 删除光标处字符 | 已实现 |
| history 内置命令 | history | 已实现 |
| 历史扩展 !! | !! -> 上一条命令 | 已实现 |
| 历史扩展 !n | !3 -> 第 3 条命令 | 已实现 |
| 历史扩展 !-n | !-1 -> 倒数第 1 条 | 已实现 |
| 历史扩展 !$ | !$ -> 上一条命令最后参数 | 已实现 |
| ~ 展开 | cd ~ 或 echo ~/path | 已实现 |
| test/[ 条件 | [ -f x ] && echo yes | 已实现（文件/字符串/数值/-a -o ! 括号） |
| 算术 $(( )) | echo $((2+3*4)) | 已实现（+ - * / % 括号，变量） |
| 位置参数 | set -- a b; echo $1 $# $@ | 已实现 |
| read 内置 | read VAR / read -r A B | 已实现（tty 回显/退格） |
| set/shift | set -- a b; shift | 已实现 |
| if/elif/else/fi | if true; then echo a; fi | 已实现（多行/单行均支持） |
| for 循环 | for i in a b; do echo $i; done | 已实现（词表支持变量/tilde/glob 展开） |
| while 循环 | while true; do ...; done | 已实现（支持 break/continue） |
| 命令替换 $() | echo $(echo hi) | 已实现（单引号内不展开，嵌套支持） |
| 别名 alias | alias ll='ls -l' | 已实现（unalias，链式展开上限 16） |
| 作业控制 jobs | jobs | 已实现（后台 & 与 Ctrl-Z 挂起） |
| 作业控制 fg/bg | fg [%n] / bg [%n] | 已实现（SIGCONT + 等待/继续） |
| Ctrl-Z | 挂起前台进程组 | 已实现（jobs 显示 Stopped） |
| 脚本模式 | sh script.sh args / sh -c 'cmd' name args | 已实现（$0/$1..、shebang） |
| 选项 | sh -e/-x/-u/-o pipefail | 已实现（set -e/-x/-u/-C/-o pipefail 同源） |
| 函数 | f() { ... } / function f { ... } | 已实现（local/return，参数独立作用域） |
| case/until | case x in ... esac / until ... done | 已实现 |
| break N/continue N | 多层循环跳出 | 已实现 |
| 参数展开运算符 | ${VAR:-def} ${#VAR} ${VAR#pat} ${VAR/old/new} | 已实现 |
| 词分割 | 未引号 `$VAR` 按 IFS 拆分；`"$VAR"` 不拆；`"$@"` 多参数 | 已实现 |
| ANSI-C 引用 | $'\n' $'\x41' $'\u4e2d' | 已实现 |
| 花括号展开 | {a,b} / {1..5} / {a..e} | 已实现（引号内不展开） |
| 算术增强 | 赋值/自增/比较/逻辑：$((x+=1)) $((a>b)) | 已实现 |
| $(<file) | 读取文件内容 | 已实现 |
| 新重定向 | &> &>> \|& <<< >&- N<&M、set -C | 已实现 |
| exec | 替换进程 + 永久重定向 | 已实现 |
| wait/disown/作业规格 | wait pid\|%job、disown、%+ %- %?str、kill %1 | 已实现 |
| trap | EXIT/INT/TERM/... 陷阱 | 已实现（脚本与交互） |
| read 选项 | -r -s -t -n -d -p | 已实现 |
| 交互配置 | PS2/PS4/IFS/HISTFILE/HISTSIZE、~/.profile、cd -、umask/let/times | 已实现 |

### 实现细节

**输入模式**：逐字节读取 stdin（普通字符按 UTF-8 首字节聚合多字节序列），支持以下按键：

| 按键 | 功能 |
|------|------|
| Tab | 命令/文件补全 |
| Enter | 执行当前行 |
| DEL/BS | 删除光标前一个字符 |
| Ctrl-D | 空行时退出 shell |
| Ctrl-C | 中断当前行，清空并新起提示符 |
| Ctrl-L | 清屏并重绘当前行 |
| Ctrl-A / Home | 跳到行首 |
| Ctrl-E / End | 跳到行末 |
| Ctrl-U | 删除光标前所有内容 |
| Ctrl-K | 删除光标后所有内容 |
| Ctrl-W | 删除光标前一个单词 |
| Up/Down | 翻阅命令历史 |
| Left/Right | 移动光标（UTF-8 字符边界对齐） |
| Delete | 删除光标处字符 |
| ESC [N~ | Home/End/Delete 的数字编码变体 |

**命令历史**：`history: Vec<String>` 存储已执行命令（非空且与最后一条不同才入栈）。`source`/`/etc/profile` 中的行**不进入交互历史**（与 bash 一致），避免污染 `history` 输出与 `!n` 历史索引。上键（\x1b[A）向上翻阅历史，下键（\x1b[B）向下翻阅，回到最新后恢复原始行。进入历史模式前保存当前行（`saved_line`），退出历史模式时恢复。

**光标移动**：维护 `cursor: usize`（字节偏移），左键（\x1b[D）/右键（\x1b[C）移动时按 UTF-8 字符边界对齐。插入/删除字符在光标处操作，而非末尾。`redraw()` 用 `\r\x1b[K` 清除当前行后重绘，并用 `\x1b[NC` 将光标定位到正确位置。

**分词器**（tokenize）将输入行切分为 Token 序列：

```rust
enum Token {
    Word(String),     // 普通参数
    RedirOut,         // >
    RedirAppend,      // >>
    RedirIn,          // <
    Pipe,             // |
    Semicolon,        // ;
    AndIf,            // &&
    OrIf,             // ||
    Background,       // &
}
```

**命令列表构建**（build_command_list）：将 Token 序列解析为 CommandList，由多个 LogicalSegment 组成，每个含一条 Pipeline 和一个 Connector（Start/Sequential/AndIf/OrIf）。

**变量展开**（expand_vars）：在分词后、执行前展开 $VAR、${VAR}、$?（退出码）、$$（PID）。

**历史扩展**（expand_history）：在分词前对原始行做文本替换。history 在 `execute_line` 之后入栈，保证 `!!` 引用上一条命令而非当前行。支持：
- `!!` -> 上一条命令
- `!n` -> 第 n 条命令（1-based）
- `!-n` -> 倒数第 n 条
- `!$` -> 上一条命令的最后一个参数

**~ 展开**（expand_tilde）：在变量展开后、通配符展开前执行。`~` 或 `~/path` 展开为 $HOME。

**别名展开**（alias）：分词前按“命令位置首词”做文本替换（行首、`;` `|` `&&` `||` `&` 之后），单/双引号内不展开，链式展开最多 16 次防循环。内置命令 `alias`/`unalias` 维护全局别名表。

**命令替换**（expand_command_subst）：分词前扫描 `$(...)`（单引号内不展开，支持嵌套与引号内括号），内层命令通过 `capture_output` 子进程执行并捕获 stdout，去尾部换行后拼回原行；不做二次语法解析（与 POSIX 接近）。内置命令（cd/export 等）在子进程中不生效（子进程语义）。

**复合命令**（compound）：REPL 用 `nesting_delta` 判断块是否完整（`if`/`for`/`while` +1，`fi`/`done` -1），未闭合时继续以 `> ` 提示读取；完整后交给 `execute_block`。块先按引号外 `;` 规范化（`if a; then b; fi` 与多行写法等价，`then`/`do`/`else`/`fi`/`done` 行首关键字独立成行），再递归解析执行，块内普通行仍由 `execute_line` 执行。`break`/`continue` 用内部退出码哨兵实现（支持嵌套，仅单层）；缺失 `then`/`do`/终结符、`elif` 在 `else` 之后、重复 `else` 均报语法错误（rc=2），顶层 `break`/`continue` 仅警告。

**作业控制**（jobs）：后台 `&` 与挂起作业统一放入独立进程组并登记作业表；`jobs` 列出（kill(pgid,0) 存活探测自动清理），`fg` 取出并 SIGCONT + 等待，`bg` 标记运行并 SIGCONT。Ctrl-Z（0x1A）由 stdin 监控线程转发 SIGTSTP 到前台进程组，前台等待用 `waitpid(..., WUNTRACED)` 感知停止并登记 Stopped 作业。SIGCHLD 在 spawn 到等待全程屏蔽（SigchldGuard），避免 std 的 `Child::wait()` 在 exec 失败路径上与处理器抢收导致 ECHILD panic。

**通配符展开**（expand_glob）：对含 * ? [] 的词项执行 glob 匹配，隐藏文件不匹配 *（与 bash 一致）。展开后按字典序排序。

**Tab 补全**（tab_complete）：
- 判断当前词是命令位置还是参数位置
- 命令位置：行首、管道 `|` 后、分号 `;` 后、`&&` / `||` 后——匹配内置 applet 名 + 内置命令（cd/exit/export/unset/pwd）+ PATH 下可执行文件
- 参数位置：其他情况——匹配文件系统路径，目录自动追加 /，多匹配列表只显示文件名
- 唯一匹配：补全 + 尾随空格（目录不加空格）
- 多匹配：补全公共前缀；无公共前缀时列出所有选项
- 路径形式（含 / 如 /bin/ls）：走文件补全而非命令补全
- 根路径补全（`cd /pro` -> `/proc/`）：`complete_file` 从 `path.file_name()` 提取搜索前缀，从 `path.parent()` 提取搜索目录；根目录的 parent 用 `/` 而非 `//`

**执行器**（execute_pipeline）：用 `Stdio::piped()` 串联子进程；后台运行（&）不等待子进程。

命令查找（resolve_command）：含 / 按字面路径，否则在 PATH 下查找可执行文件。查找失败时回退到当前可执行文件路径 `rbox <cmd>` 内置 applet。

重定向文件（`>`/`>>`/`<`）打开失败时打印错误并返回非零退出码，不会静默丢弃输出。

默认 PATH 由 init（PID 1）启动时统一设置，shell 直接继承，不再自行设置。

### 测试

集成测试在 `tests/run_tests.sh` 中，通过 QEMU 全系统模拟运行所有命令。共 41 个测试组、261 个断言（涵盖 65 个 applet、Shell 全功能、init 服务管理、Wants/Requisite/Before 依赖、emergency/single 启动模式、rescue 降级、持久盘 switch_root、rgetty/rlogin 登录与超时流程、重启/关机流程）：

| 测试组 | 测试项 | 数量 |
|--------|--------|------|
| 基本 applet | uname -m、uname -n、pwd、echo、cat | 5 |
| 文件操作 | 重定向写入、cp、ls | 3 |
| 管道与重定向 | cat\|cat、追加写入 | 3 |
| init 启动流程 | PID 1、fstab、挂载、加载单元、reached target | 5 |
| 服务管理 | Environment、Restart=on-failure、status 查询 | 6 |
| rservice 管理 | stop、start、restart、list | 4 |
| init 增强 | ExecReload、sysctl、User= 降权、forking、kmsg | 8 |
| 引号与转义 | 双引号、单引号、反斜杠、续行、注释 | 5 |
| 变量展开 | $VAR、${VAR}、$?、$$、unset | 5 |
| 控制操作符 | ;、&&、\|\|、链式、后台 & | 5 |
| 重定向 | >、>>、< | 3 |
| 管道 | 3 级管道、管道+重定向 | 3 |
| 通配符 | *、?、[] | 4 |
| 历史扩展 | !!、!n、!$、history | 4 |
| ~ 展开 | echo ~、cd ~ + pwd | 2 |
| Tab 补全 | 命令、文件、管道后 | 3 |
| 行编辑快捷键 | Ctrl-E/U/K/W/C | 5 |
| UTF-8 输入 | 中文等多字节字符端到端 | 1 |
| 后台/前台退出码 | 后台命令与前台命令并发时退出码正确 | 1 |
| 文本处理 applets | head、printf、wc、grep、basename、dirname、date、env、ln、echo -n、ls -a/-1、rm -r、touch、mkdir -p、tail | 18 |
| stderr 重定向 | 2>、2>> | 2 |
| source 命令 | source 加载变量 | 1 |
| PS1 提示符 | PS1 设置执行 | 1 |
| here-doc | <<EOF | 1 |
| console respawn | 初始环境变量、exit 后 respawn 保留配置 | 2 |
| 前台 Ctrl-C 中断 | sleep 被 SIGINT 中断，$?=130 | 1 |
| rgetty/rlogin 登录 | 登录提示、issue 横幅、错误密码拒绝、登录后 shell 可用、串口指定、退出后重新登录 | 6 |
| rgetty/rlogin 超时 | 持续输入不超时、空闲超时登出、密码输入超时、超时后重新登录 | 4 |
| 重启流程 | reboot 触发有序关机、重启后系统恢复 | 2 |
| 关机流程 | shutdown 触发、ExecStop 逆序、power off | 3 |
| 内存信息/进程树 | meminfo 输出、分类核算、iomem 树、processes 进程树 | 15 |
| Shell: 引号保护 glob / 内置重定向 | 双引号 * 不展开、内置 pwd 重定向 | 3 |
| 新增 applet | chmod/chown/find/kill/dmesg/mount/umount | 16 |
| Shell: 复合命令/别名/命令替换/作业控制 | if/for/while、alias、$()、jobs、Ctrl-Z | 8 |
| Shell: 脚本模式/POSIX 展开/新重定向 | sh 脚本/-c/-e、函数/case/until/break N、参数展开、词分割、$'...'、&>/<<</\|&、read/wait | 18 |
| rescue 启动降级 | target Requires 失败 → 停止服务进 rescue shell | 4 |
| 持久盘模式 | switch_root、写入、重启后数据保留 | 3 |
| Shell: ash 对齐与覆盖补齐 | 子 shell/花括号组/`!`/反引号/`:`/readonly/getopts/ulimit、位运算与三元、参数子串、`for` 无 in、复合重定向、任意 fd/`<>`/`>|`、noclobber、`$-`/`set -o`/`$RANDOM`/`set -f`、CDPATH、`cd -L/-P`、`pwd -P`、`kill %job`/`kill -l` 表格、`&&`/`||` 组、`$ENV`、ignoreeof（Ctrl-D） | 44 |
| **合计** | | **261** |

> **注意**：Ctrl-A (0x01) 在 QEMU `-nographic` 模式下是 monitor 转义前缀，不会传递给客户机，因此无法在自动化测试中覆盖。Ctrl-A 在交互式 `make run` 中可正常使用（宿主机 stty raw 模式下传递）。

**已知限制**（按性质分类）：

*非缺口（BusyBox ash 本身不支持，属对齐目标之外）*：
- 进程替换 `<()`/`>()`（bash/ksh 扩展，ash 无）
- 数组、`declare`/`typeset`、`[[ ]]`、`${var//pat/rep}`（bash 扩展，ash 无）
- Ctrl-R 反向历史搜索、kill ring/撤销（BusyBox ash 行编辑无此功能；ash 支持
  Ctrl-A/E/K/U/W、上下键历史、Tab 补全）
- Ctrl-A 被 QEMU `-nographic` 截获（测试基础设施限制，交互式 `make run` 正常）

*POSIX 语义说明（非缺陷）*：
- 命令替换 `$()` 在子 shell 中执行，其中的 `cd`/变量赋值不影响父 shell（POSIX 规定）

*真实缺口（ash/POSIX 支持，本实现暂缺，已列入后续计划）*：
- 管道段子 shell `cmd | ( ... )` 已实现（`rbox --subshell` 子进程 + tokenizer 整体捕获
  `( ... )`）；子进程继承环境变量/cwd/umask，函数/别名/位置参数经状态编码参数传递
  （`\x1e`/`\x1f`/`\x1d` 分隔），已与 ash 实测一致；REPL 支持单行/多行函数定义

- `<<<`/`|&` 与别名脚本展开按 busybox ash 实测对齐（`<<<`/`|&` 语法错误；
  别名仅交互式展开）；混合引号词分割已按 POSIX 修复：`expand_vars` 对未加引号展开值包裹 `SPLIT_ESCAPE`
  标记，`split_marked` 仅拆分标记区间；tokenizer 在双引号结束处补边界标记，
  避免 `"$x"suf` 的变量名吞掉 `suf`（已与 busybox ash 实测一致）

### 终端模式（Tab 补全的前提）

Tab 补全要求 shell 能逐字节读取按键，但默认终端处于 **canonical（行缓冲）模式**，按下 Tab 不会立即传递给进程。因此需要两层终端设置：

1. **客户机侧（shell 启动时）**：`enable_raw_mode()` 通过 `libc::tcgetattr` 保存原始终端属性，`tcsetattr` 设置 cbreak 模式（关闭 `ICANON` + `ECHO` + `ISIG`，使 Ctrl-C 作为 `0x03` 字节传递，`VMIN=1 VTIME=0`）。`RawGuard` 在 shell 退出时通过 `Drop` 自动恢复。管道输入时 `tcgetattr` 失败返回 `None`，不影响。
2. **宿主机侧（make run / run.sh）**：`stty -echo -icanon min 1 time 0` 将宿主机终端设为 raw 模式，让按键立即传递给 QEMU。QEMU 退出后 `stty sane` 恢复。

```rust
struct RawGuard { fd: i32, original: libc::termios }
impl Drop for RawGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original); }
    }
}
```

### Ctrl-C 前台进程中断

raw 模式下 **ISIG 已关闭**，Ctrl-C 不产生 SIGINT 信号，而是作为 `0x03` 字节到达 stdin。shell 主线程在 `wait()` 中阻塞等待子进程，无法读 stdin，因此使用一个**后台监控线程**轮询 stdin 的 `0x03` 字节：

1. `execute_pipeline()` 设置 `FOREGROUND_PGID`（第一个子进程的 pid）
2. 启动 stdin 监控线程，非阻塞 read stdin：
   - 收到 `0x03` -> `kill(-pgid, SIGINT)` 转发给子进程组，线程退出
   - 收到其他字节 -> 缓存到共享 pending 队列（保序不丢失），供 shell 后续读取
3. shell 主线程 `wait()` 子进程退出（信号终止的退出码取 `128 + signal`，如 SIGINT -> 130）
4. 设置 stop_flag 停止监控线程，`join()` 等待退出；REPL 读取 stdin 前**先消费 pending
   队列**（TIOCSTI 推回追加到 tty 队列队尾会乱序/错位，已弃用）
5. 清除 `FOREGROUND_PGID`，如果退出码为 130 则打印换行

子进程在 `pre_exec` 中通过 `setpgid(0, 0)` 创建独立进程组，同时恢复 SIGINT/SIGQUIT/SIGTSTP 为 `SIG_DFL`（不继承 shell handler）。

如果 shell 在编辑行时（无前台子进程），REPL 的 `0x03` 分支直接清除当前行并显示新提示符。

SIGINT handler 仍注册为后备（管道模式下 ISIG 仍然开启时生效）。

### SIGCHLD 后台进程回收

shell 注册 `SIGCHLD` handler，自动回收后台子进程（`&` 启动的），避免僵尸进程。handler 内部循环 `waitpid(-1, WNOHANG)` 直到无子进程可回收。前台命令在 spawn 前用 `SigchldGuard`（`pthread_sigmask`）屏蔽 SIGCHLD，直到等待结束后恢复：避免 handler 用 `waitpid(-1)` 抢收前台子进程，导致 `waitpid(pid)` 返回 ECHILD；同时消除 `Command::spawn()` exec 失败路径上 std 内部 `Child::wait()` 与处理器抢收的 panic 竞态。

### 命令历史持久化

shell 启动时从 `~/.rbox_history` 加载历史记录，每条命令执行后追加写入。历史文件路径优先使用 `$HOME/.rbox_history`，`$HOME` 未设置时使用 `/tmp/.rbox_history`。

### PS1 提示符

shell 支持 `$PS1` 环境变量自定义提示符，支持的转义序列：`\u`（用户名）、`\h`（主机名）、`\w`（当前路径，`$HOME` 替换为 `~`）、`\$`（`$`）、`\#`（`#`）、`\n`（换行）、`\e`（ESC）、`\\`（反斜杠）。`$PS1` 未设置时默认为简约的 `> `（`/etc/rbox.conf [shell] default_ps1` 可改）。

`/etc/profile` 在 shell 启动时自动 source，设置默认 PATH、USER、HOSTNAME、PS1 等。

### source 内置命令

`source file` 或 `. file`：逐行读取文件并执行，跳过空行和 `#` 注释行。不支持 `exit`（source 中直接 return）。被 source 的行不会写入交互式历史/历史文件。

### stderr 重定向

支持 `2>`（覆盖）和 `2>>`（追加）重定向 stderr。在 tokenizer 中通过前导 `2` 识别，parser 设置 `SimpleCmd.stderr_file`，executor 在 `command.stderr()` 中配置。

### here-doc

支持 `<<EOF` 语法。REPL 检测到 `<<` 后，提示 `> ` 逐行读取内容直到遇到 delimiter，写入临时文件 `/tmp/heredoc_<pid>`，然后替换为 `< /tmp/heredoc_<pid>` 执行。仅在交互式 tty 模式下可用。

文件：src/applets/core/shell/executor.rs、src/applets/core/shell/reader.rs、src/applets/core/shell/mod.rs

## 终端登录（rgetty / rlogin）

文件：src/applets/core/rgetty.rs、src/applets/core/rlogin.rs

生产 rootfs 的 console 服务由原来的直接 shell 改为 `rgetty`，登录流程如下：

```
init ──Restart=always 服务──► rgetty（常驻）──fork──► rlogin ──exec──► 用户 shell
        ▲                        │     ▲                    │
        │                        │     └──── 登录失败/shell 退出 ────┘
        │                        │            （rgetty 原地重新提示）
        └──── rgetty 崩溃/被杀时才由 init 按 Restart=always 重启 ────┘
```

- **rgetty**：用法 `rgetty [-L] [-t SEC] [TTY]`。
  - `-L`：设置 CLOCAL（忽略载波检测，真实串口常用，同 busybox getty）；
  - `-t SEC`：**登录会话空闲超时**（无输入达到 SEC 秒自动登出，从登录成功/进入
    shell 开始计时，登录提示与密码阶段不超时；有输入活动会刷新计时）；
  - 每次提示前打印 `/etc/issue` 横幅（路径可配置，不存在则跳过；登录失败/退出后
    重新提示时横幅重现，同 busybox getty）；
  - 忽略 SIGINT/SIGQUIT：登录提示与密码阶段的 Ctrl-C/\ 不打断登录链路
    （shell 会话阶段由 shell 自己接管 SIGINT，不受影响）；
  - 打印提示（`/etc/rbox.conf [getty] prompt`）后读取用户名，**fork 子进程**执行
    `/bin/rlogin <user>`（路径可配置）；父进程 wait：登录失败（非零退出）按
    `failure_delay` 延迟后原地重新提示，shell 正常退出立即重新提示；
    rgetty 本身常驻，只有崩溃/被杀才由 init 的 `Restart = "always"` 拉起。
  **终端选择**：仅使用命令行显式 TTY 参数（由 `ExecStart` 完整命令直接传给 rgetty）；
  未指定时使用继承的 stdin/stdout/stderr（init 启动服务时继承的 stdio 即登录终端）。
  TTY 可写裸设备名（`ttyAMA0`）或 `/dev/` 路径。启动时会把终端恢复为行缓冲模式。
- **rlogin**：无参数时先提示用户名（提示文本可配置）；随后关闭回显读取密码。密码读取带超时
  （`/etc/rbox.conf [login] password_timeout`，默认 60s，0 = 不超时；超时输出 `Password timed out`
  并退出，防恶意用户挂住登录进程）；支持退格键删除已输入字符，密码长度上限 256 字节
  （超限拒绝登录）。密码校验规则：
  - `/etc/passwd`（路径可配置）密码字段为 `x` 时读取 `/etc/shadow`（路径可配置）；
    空字段 = 免密登录，`!`/`*` 开头 = 账户锁定；
  - 存储串以 `$` 开头（`$5$...` 等）：用 libc crypt() 校验（与 glibc/busybox 兼容，
    支持 SHA-256/SHA-512/MD5）；其余按明文比对（兼容旧格式）。
  - 校验失败输出 `Login incorrect` 并直接退出（失败延迟由常驻的 rgetty 处理）；
  - 校验通过后：initgroups/setgid/setuid 降权、chdir 到 home（失败回退 `/`）、设置
    USER/LOGNAME/HOME/SHELL 环境变量、打印 MOTD（路径可配置），最后 exec 用户 shell
    （passwd 无 shell 字段时用配置的缺省 shell）。

生产 rootfs 的账号数据：`/etc/passwd`（root/nobody，密码字段为 `x`）+
`/etc/shadow`（root 密码为 SHA-256 crypt 哈希，明文是 `root`；nobody 锁定 `!`）。
rootfs 携带 `libcrypt.so.1`（由 Makefile 从交叉工具链拷贝，rlogin 的 crypt 校验依赖）。

生产 console 单元示例（getty 参数直接写在 ExecStart 完整命令中，登录进程退出后由
`Restart = "always"` 无条件重启）：

```toml
[Service]
Type = "simple"
ExecStart = "/bin/rgetty -L -t 60 ttyAMA0"   # -L + 超时 + 登录终端
Restart = "always"
RestartSec = 1
```

## 全局配置（/etc/rbox.conf）

文件：src/config.rs

所有"环境相关"的硬编码集中到 `/etc/rbox.conf`（TOML，可选字段，缺省用代码内默认值，进程内只解析一次）。字段一览：

| 分组 | 字段 | 默认值 | 使用方 |
|------|------|--------|--------|
| [paths] | system_dir | /etc/rbox/system | init 单元目录 |
| [paths] | default_target | default.target | 启动根 target |
| [paths] | status_socket | /run/rbox.sock | 控制协议 socket（/run 为 tmpfs root 目录，创建后 chmod 600，仅 root 可连） |
| [paths] | passwd / shadow | /etc/passwd / /etc/shadow | rlogin 账号校验 |
| [paths] | motd | /etc/motd | 登录后欢迎信息 |
| [paths] | profile | /etc/profile | shell 启动时 source |
| [paths] | history_file | 空 = $HOME/.rbox_history | shell 历史（支持 ~ 前缀） |
| [paths] | fstab / hostname / sysctl_conf | /etc/fstab 等 | init 系统初始化 |
| [paths] | meminfo / iomem / proc | /proc/meminfo 等 | meminfo 命令数据源 |
| [getty] | login_program | /bin/rlogin | rgetty fork 的登录程序 |
| [getty] | prompt | "rbox login: " | 登录提示（生产示例为极简 "user: "） |
| [getty] | timeout_message | "Session timed out, logging out — time flies when you're idle!" | 会话空闲超时登出消息（俏皮默认，可自定义） |
| [getty] | default_timeout | 无 | 未给 -t 时的默认空闲超时 |
| [getty] | issue_file | /etc/issue | 登录前横幅 |
| [getty] | failure_delay | 1 | 登录失败后重新提示延迟 |
| [login] | shell | /bin/sh | passwd 缺 shell 字段时缺省 |
| [login] | password_prompt | "Password: " | 密码提示（生产示例为极简 "passwd: "） |
| [login] | password_timeout | 60（0 = 不超时） | rlogin 密码输入超时秒数 |
| [login] | password_timeout_message | "Password timed out — daydreaming at the login prompt?" | 密码超时消息（俏皮默认，可自定义） |
| [init] | default_path | /bin:/sbin:/usr/bin:/usr/sbin | init 启动时设置的默认 PATH |
| [init] | watchdog_path | /dev/watchdog | 硬件看门狗设备（打开失败静默禁用） |
| [init] | watchdog_interval | 10 | 喂狗间隔秒（须小于硬件超时；0 = 不启用） |
| [shell] | default_ps1 | "> " | 未设置 $PS1 时的默认提示符 |

生产 rootfs 内置一份带注释的 `/etc/rbox.conf` 作为示例。

一个 systemd 风格的 PID 1 初始化进程，使用 TOML 格式的单元文件配置。

### 配置文件

单元文件放在 `/etc/rbox/system/*.toml`，使用 systemd 风格的三段式结构：

```toml
# /etc/rbox/system/hello.service.toml

[Unit]
Description = "Hello service"
Name = "hello"                        # 可选：单元名（rservice/status/依赖引用用它；缺省回退文件名）
After = ["network.service"]        # 可选：在此服务之后启动
Requires = ["network.service"]     # 可选：硬依赖（失败则本单元跳过）
Wants = ["log.service"]            # 可选：尽力依赖（参与排序，失败不传播）
Requisite = ["db.service"]         # 可选：前置检查（不激活依赖；未成功则本单元跳过）
Before = ["late.service"]          # 可选：本单元必须先于这些单元启动

[Service]
Type = "simple"                    # simple（默认）/ forking（daemon 化）
ExecStart = "/bin/rbox echo hello" # 启动命令
ExecStop = "/bin/rbox echo bye"    # 可选：关机时执行的停止命令
ExecReload = "/bin/rbox echo ok"   # 可选：rservice reload 执行的命令
Environment = ["HELLO=world"]      # 可选：服务环境变量
EnvironmentFile = "/etc/x.env"     # 可选：环境变量文件（前缀 - 表示缺失不报错）
WorkingDirectory = "/var/lib/x"    # 可选：工作目录
Restart = "on-failure"             # 可选：非零退出自动重启（默认 no）
RestartSec = 1                      # 可选：重启间隔秒（默认 1）
StartLimitBurst = 5                 # 可选：失败/重启上限（默认 5；burst 为允许的重启次数，第 burst+1 次失败放弃）
TimeoutStartSec = 10                # 可选：forking 等待父进程退出超时（默认 10）
TimeoutStopSec = 5                  # 可选：停止时 SIGTERM 等待秒数（默认 5）
KillMode = "control-group"          # 可选：control-group/process/mixed/none
PIDFile = "/var/run/x.pid"         # 可选：forking 的 daemon PID 文件
LogFile = "/var/log/x.log"         # 可选：stdout/stderr 重定向文件（打不开仅告警并回退 console，不阻止启动）
User = "nobody"                    # 可选：降权用户（getpwnam）
Group = "nogroup"                  # 可选：降权组（getgrnam）
Restart = "always"                 # 可选：退出即重启（console/getty 用；另有 on-failure）

[Install]
WantedBy = ["default.target"]      # 被哪个 target 拉入
```

target 文件（如 default.target.toml）本身不含 ExecStart，仅作为依赖图的根节点。

**单元命名**：`[Unit] Name = "..."` 显式声明单元名（rservice/status/依赖引用均使用它）；缺省时回退文件名（去掉 `.toml`）。target 类型按文件名 `.target` 后缀判定，不受 Name 影响。

### 启动流程

1. **信号处理**：安装 SIGTERM/SIGINT/SIGCHLD 处理器（SIGTERM 设关机标志，SIGINT 设重启标志，SIGCHLD 唤醒主循环）；SIGHUP/SIGPIPE/SIGQUIT 显式忽略——PID 1 不能被这些信号终止（tty 断开/写断管道/终端转义符一旦命中即 kernel panic）
2. **环境与挂载**：设置默认 PATH（shell/服务子进程继承）；读取 /etc/fstab 逐个挂载（缺失时回退内置默认集：proc/sysfs/devtmpfs/devpts/tmpfs//run）；early 阶段先试读 /proc/cmdline，失败才挂载 proc；读取 /etc/hostname 设置主机名（sethostname）
3. **加载单元**：解析 /etc/rbox/system/*.toml，serde 反序列化
4. **拓扑排序**：从 default.target 出发 DFS，Requires=/After= 构成边，WantedBy= 构成反向依赖（target 拉入所有 WantedBy 它的服务），含环检测
5. **启动服务**：按依赖深度分层（同层无依赖边），逐层并发 fork+exec ExecStart（独立进程组，带 Environment），按拓扑顺序合并结果（services 顺序保持启动顺序，ExecStop 逆序语义不变），记录 Child 句柄和 ExecStop
6. **常驻**：主循环回收服务进程（try_wait，避免僵尸）；`Restart=on-failure` 非零退出自动重启、`Restart=always` 退出即重启（固定 RestartSec 间隔 + StartLimitBurst 上限）；**waitpid(-1) 收割收养的孤儿进程**（防僵尸累积）；通过 `/run/rbox.sock` 响应控制请求（`status`/`start`/`stop`/`restart`/`reload`，供 rbox status / rservice 使用；控制连接在独立线程处理，ExecStop 数秒级操作不再阻塞主循环）；检测关机标志

`Type=` 目前支持 `simple` 与 `forking`；其他值会打印警告并按 simple 处理。
`Restart=` 目前支持 `no`（默认）、`on-failure` 与 `always`，其他值打印警告并按 no 处理。

关机时按进程组（`process_group(0)`）SIGTERM 服务及其后代进程，1 秒超时后 SIGKILL，不再只杀直接子进程。全部停产后 sync_fs 并尝试将根文件系统 remount 只读再触发 reboot（initramfs 根不可 remount 时忽略失败）。

### 控制协议（/run/rbox.sock）

单行请求，文本响应：

| 请求 | 说明 |
|------|------|
| `status` / 空 | **tree 风格**树形输出：`init` 在最顶（带自身 cpu/mem），各服务以 `├──`/`└──` 分支（含未启动的 not-started，运行实例带 pid/重启策略/失败计数 failed=N/burst），运行服务下挂进程树（pid/名称/状态/CPU%/内存，`│` 延续）；CPU% 由控制线程对 /proc 双采样（间隔 300ms）计算 |
| `status <unit>` | 查询单个单元（target 显示 `target`；未运行单元显示 not-started） |
| `start <unit>` | 启动服务（已停止的重新拉起；未启动过的从单元文件新建） |
| `stop <unit>` | 停止服务（执行 ExecStop + SIGTERM 进程组，超时 SIGKILL；标记 stopped 禁止自动重启） |
| `restart <unit>` | 停止后重新启动 |
| `reload <unit>` | 执行 ExecReload 命令（不重启进程） |

客户端：`rbox status` / `rservice`（list/status/start/stop/restart/reload）。所有服务（含 console/getty）统一管理，无特殊保护。

### 关机/重启流程

```
shutdown 命令 / SIGTERM ──► 关机（设置 SHUTDOWN_REQUESTED 标志）
reboot 命令   / SIGINT  ──► 重启（设置 REBOOT_REQUESTED 标志）
  |
  +-- 设置对应全局标志
  +-- do_shutdown():
  |     +-- 逆序遍历已启动的服务，执行 ExecStop + SIGTERM 等服务退出
  |     |    （单服务 1 秒超时后 SIGKILL 强杀，避免挂起）
  |     +-- kill(-1, SIGTERM) -> 所有残留进程
  |     +-- 收割循环（50ms 轮询，受总 deadline 约束）
  |     |    （总超时 10s 到点后 kill(-1, SIGKILL) 强制清理，再给 1s 收割窗口）
  |     +-- sync()
  |     +-- reboot(RB_POWER_OFF)（关机）或 reboot(RB_AUTOBOOT)（重启）
  +-- QEMU 退出 / 重启
```

### 系统调用封装

所有系统调用通过 libc crate 调用（不直接写 extern FFI）：

| 函数 | 用途 | 使用位置 |
|------|------|----------|
| libc::mount | 挂载 /etc/fstab 列出的文件系统 | init.rs |
| libc::sethostname | 设置主机名（/etc/hostname） | init.rs |
| libc::waitpid | 收割收养的孤儿进程（WNOHANG） | init.rs |
| libc::sigaction | 注册 SIGTERM/SIGINT 处理器（SA_RESTART） | init.rs |
| libc::kill | 向进程/所有进程发送信号 | init.rs, shutdown.rs, reboot.rs |
| libc::sync | 刷新文件系统缓冲 | init.rs |
| libc::reboot | 关机 (RB_POWER_OFF) / 重启 (RB_AUTOBOOT) | init.rs |
| libc::uname | 获取系统信息 | uname.rs |
| libc::time / localtime_r | 获取时间 | date.rs, ls.rs |
| libc::utimensat | 设置文件时间戳 | touch.rs |
| libc::dup2 | rgetty 把指定 tty 复制到 stdin/stdout/stderr | rgetty.rs |
| libc::tcgetattr / tcsetattr | rgetty 恢复终端模式/CLOCAL；rlogin 关闭密码回显 | rgetty.rs, rlogin.rs |
| libc::poll | rgetty 带超时读取用户名 | rgetty.rs |
| crypt(3)（libcrypt） | rlogin 校验 $5$/$6$ 等密码哈希 | rlogin.rs |
| libc::initgroups / setgid / setuid | rlogin 登录后降权 | rlogin.rs |

libc::reboot 使用 glibc 封装的简化签名 `reboot(how_to)`，不需要手动传递 magic number。

### 当前 TOML 单元文件（生产 rootfs）

| 文件 | 类型 | Name | 说明 |
|------|------|------|------|
| default.target.toml | target | default.target | 启动根节点（无 Name 字段，回退文件名） |
| console-shell.service.toml | service | console-shell | ExecStart=/bin/rgetty -L -t 60 ttyAMA0，Restart=always 登录提示（登录成功后 exec shell） |
| logkeeper.service.toml | service | logkeeper | ExecStart=/bin/rbox logkeeper，Restart=always 将 /dev/kmsg 转发到 /var/log/messages |

测试专用服务（hello、restart-test、longrun、forktest、forktimeout、usertest、console-shell 覆盖单元）放在 `tests/units/`，由集成测试脚本运行时注入 rootfs 并打包独立的测试 initramfs，测试结束自动清理并恢复被覆盖的生产单元，不进入生产镜像。

### fstab 挂载表

init 启动时读取 `/etc/fstab` 逐个挂载文件系统（标准五/六字段格式，`#` 注释与空行忽略）；文件缺失时回退到内置默认集：

```fstab
# <device> <mountpoint> <type> <options> <dump> <pass>
proc     /proc      proc      defaults  0 0
sysfs    /sys       sysfs     defaults  0 0
devtmpfs /dev       devtmpfs  defaults  0 0
devpts   /dev/pts   devpts    defaults  0 0
tmpfs    /tmp       tmpfs     defaults  0 0
```

options 支持常见标志（逗号分隔）：`ro`/`remount`/`noexec`/`nosuid`/`nodev`/`noatime`/`sync`，`defaults` 与未知选项视为 0。单个挂载失败仅记录日志，不中断其余挂载。
## 构建系统

文件：Makefile

### 快速开始

```bash
make all       # 编译 + rootfs + initramfs（一步到位）
make run       # QEMU 启动（需要内核已编译）
make test      # 集成测试
```

### 全部目标

| 命令 | 说明 |
|------|------|
| make all | 编译 + 构建 rootfs + 打包 initramfs |
| make build | 交叉编译 rbox（cargo build --target aarch64-unknown-linux-gnu --release） |
| make rootfs | 拷贝 rbox 二进制 + 创建 65 个 applet 符号链接 + 拷贝 glibc 运行时 |
| make initramfs | 将 rootfs/ 打包为 initramfs.cpio.gz（newc 格式 + gzip） |
| make run | QEMU 全系统模拟启动（initramfs） |
| make disk | 制作 ext4 磁盘镜像（rootfs.ext4，mkfs.ext4 -d） |
| make run-disk | 从 ext4 磁盘镜像启动（root=/dev/vda 触发 switch_root） |
| make strip | strip 符号表（减小体积；release profile 已开 strip） |
| make rootfs-test | 构建含测试单元的 initramfs（不污染生产 rootfs） |
| make kernel | 编译 ARM64 内核（defconfig + Image） |
| make clean | 清理产物 |
| make help | 显示帮助 |

### QEMU 启动参数

```bash
qemu-system-aarch64 \
    -M virt \
    -cpu cortex-a72 \
    -m 128M \
    -nographic \
    -kernel kernel/arch/arm64/boot/Image \
    -initrd initramfs.cpio.gz \
    -append "console=ttyAMA0 rdinit=/init"
```

- -M virt：QEMU virt 虚拟机
- -cpu cortex-a72：ARM Cortex-A72 CPU
- -nographic：纯串口输出，无图形界面
- rdinit=/init：内核启动后执行 initramfs 中的 /init（-> bin/rbox -> init applet）

### 元命令

rbox 二进制本身支持的元命令（非 applet）：

| 命令 | 说明 |
|------|------|
| rbox --list | 列出所有 applet 名称 |
| rbox --help / rbox -h | 显示用法 + applet 列表 |
| rbox --version / rbox -V | 显示版本号 |
## 测试

文件：tests/run_tests.sh

集成测试通过单次 QEMU 启动运行所有测试命令，捕获输出并用 grep 断言。

测试专用服务单元（`tests/units/`）在脚本运行时注入 `rootfs/etc/rbox/system/`，打包独立的 `initramfs.test.cpio.gz` 供 QEMU 使用；测试结束（含中断）通过 trap 自动清理注入文件与测试镜像，生产 rootfs 与 `make run` 用的 `initramfs.cpio.gz` 保持干净。

### 测试覆盖

集成测试共 41 个测试组、261 个断言，覆盖全部 65 个 applet 及 Shell/init/重启/关机流程，
完整分组与数量见上文「已实现的 Applet」中的集成测试表格。运行结果以 `tests/run_tests.sh`
末尾的汇总为准（`结果: N 通过, 0 失败`）。

单元测试（793 个）使用 `make coverage`（cargo-llvm-cov）可生成覆盖率报告，当前整体约
73% 行覆盖 / 83% 函数覆盖。Shell 各模块行覆盖：expander 94%、tokenizer 88%、parser 99%、
compound 86%、options 85%、alias 96%、trap 94%、jobs 78%、completion 81%、reader 77%、
script 65%、builtin 62%、executor 55%、mod（REPL 主循环）26%。REPL 主循环、fork/exec 子
进程路径主要由 QEMU 集成测试覆盖（覆盖率工具只统计宿主机单测，不计入 QEMU 运行）。

### 运行测试

```bash
make test
# 或
bash tests/run_tests.sh
```

### 单元测试（宿主机）

核心解析/排序逻辑附带 `#[cfg(test)]` 单元测试，在宿主机（x86_64）直接运行，无需 QEMU：

```bash
make unittest
```

| 模块 | 覆盖 | 数量 |
|------|------|------|
| shell/tokenizer | tokenize（引号/转义/重定向/管道/控制操作符/注释/续行/保护标记/新重定向） | 39 |
| shell/parser | parse（逻辑段/语法错误/后台/管道/fd 复制/新重定向） | 31 |
| shell/expander | 变量/位置参数/算术/历史/tilde/glob/词分割/花括号/参数展开运算符 | 54 |
| shell/completion | find_last_word_start、complete_command、complete_file、common_prefix | 29 |
| shell/builtin | 内置命令（含 exec/wait/trap/type/read 选项等） | 21 |
| shell/reader | make_prompt（PS1）、continuation prompt（PS2）、display_width、set_isig | 16 |
| shell/executor | 重定向、命令替换、X_OK、SIGCHLD、fd 复制/关闭、here-string | 13 |
| shell/types | CommandList/Pipeline/SimpleCmd/Token 默认值与比较 | 8 |
| shell/mod | read_utf8_char、here-doc、续行、source、~ 路径展开、heredoc 检测 | 15 |
| shell/alias | 别名定义/查询/展开 | 7 |
| shell/compound | if/for/while/until/case、break N、规范化、语法错误 | 15 |
| shell/jobs | 作业表/状态/规格/回收标志 | 8 |
| shell/params | 位置参数 set/get/count/shift、$0/$! | 3 |
| shell/options | set -e/-x/-u/-C/-o pipefail 状态 | 4 |
| shell/trap | 信号陷阱设置/查询/待处理信号 | 4 |
| shell/functions | 函数表与局部变量 | 3 |
| shell/script | 脚本驱动：参数、函数定义、here-doc、case/until、-e、return | 10 |
| shell/fuzz | 随机化健壮性 | 4 |
| init/units | parse_cmdline、compute_start_order（Before）、单元字段、fstab 解析 | 18 |
| init/server | 控制协议处理、status 渲染 | 17 |
| init/services | 服务生命周期、schedule_restart、EnvironmentFile、KillMode | 13 |
| init/mount | fstab 挂载、mount_line_matches 匹配 | 7 |
| init/mod | failed_required_dep、compute_depths、root 规格解析、秒退检测 | 14 |
| init/boot | cmdline 启动模式解析 | 1 |
| init/watchdog | 喂狗超时压缩 | 4 |
| config | /etc/rbox.conf 解析 | 4 |
| text/* | grep 14、printf 12、util 9、echo 7、basename 7、tr 6、sort 6、head 6、cut 6、tail 6、wc 5、uniq 5、dirname 5、tee 4 | 98 |
| file/* | ls 14、find 11、chmod 10、util 8、chown 8、tar 7、cp 7、stat 5、rm 5、mv 5、mktemp 5、mkdir 5、touch 4、realpath 4、ln 4、df 4、dd 4、cat 4 | 122 |
| sys/* | meminfo 20、mount 12、kill 10、test 8、umount 7、processes 7、dmesg 7、sleep 6、env 6、uname 5、timeout 5、pgrep 5、passwd 5、id 4、logkeeper 3、hostname 3、uptime 3、date 2、su 2、true/false/pwd 各 1 | 123 |
| core/* | rservice 3、status 2、log 4、shutdown 1、reboot 1、control 3、rgetty 11、rlogin 12 | 37 |
| proc / glob / fstab（共享工具） | 进程信息收集/单位格式化；glob 匹配；fstab 解析 | 15 |
| main | applet 注册表唯一性/查找/--help 处理 | 7 |
| **合计** | | **261** |

测试结果示例：

```
========================================
rbox 集成测试
========================================

[基本 applet]
  PASS  uname -m -> aarch64
  PASS  uname -n -> 主机名
  ...
[rgetty/rlogin 登录流程]
  PASS  rgetty 登录提示
  PASS  错误密码被拒绝
  PASS  登录后 shell 可用
  PASS  退出后重新登录
[重启流程]
  PASS  reboot 触发有序关机
  PASS  重启后系统恢复
[关机流程]
  PASS  shutdown 触发关机
  PASS  ExecStop 逆序执行
  PASS  power off

========================================
结果: 200 通过, 0 失败
========================================
```
## rootfs 布局

rootfs/ 是最终打包进 initramfs 的根文件系统目录树。

**版本库跟踪策略**：仅 `rootfs/etc/`（fstab、hostname、TOML 单元文件）由 git 跟踪；`bin/`、`lib/`、`init` 链接等构建产物由 `make rootfs` 生成，不入库。全新 clone 后直接 `make all` 即可重建完整 rootfs。

```
rootfs/
├── init -> bin/rbox        # 内核 rdinit=/init 入口
├── bin/
│   ├── rbox                # ARM64 ELF，动态链接
│   ├── sh -> rbox          # shell 别名
│   ├── true -> rbox        # 各 applet 符号链接
│   ├── echo -> rbox
│   ├── cat -> rbox
│   ├── ls -> rbox
│   ├── ... (每个 applet 一个)
│   ├── init -> rbox
│   ├── shutdown -> rbox
│   └── reboot -> rbox
├── lib/
│   ├── ld-linux-aarch64.so.1    # glibc 动态链接器
│   └── aarch64-linux-gnu/       # multiarch 目录（glibc 默认搜索路径）
│       ├── libc.so.6            # glibc
│       ├── libcrypt.so.1        # crypt 密码校验（rlogin 依赖）
│       └── libgcc_s.so.1        # GCC 运行时
└── etc/
    ├── hostname                 # 主机名（init 启动时读取）
    ├── fstab                    # init 挂载表
    ├── passwd                   # 用户账号（密码字段为 x，实际在 shadow）
    ├── shadow                   # 影子密码（SHA-256 crypt，root 明文为 root）
    ├── motd                     # 登录成功后的欢迎信息
    ├── issue                    # 登录前横幅（rgetty 启动时打印）
    ├── rbox.conf                # 全局配置（TOML，路径/提示/超时等）
    └── rbox/
        └── system/              # init TOML 单元文件（生产：仅 default.target + console-shell）
            ├── default.target.toml
            └── console-shell.service.toml   # ExecStart=/bin/rgetty，登录成功后 exec shell
```

glibc 运行时从交叉编译器的 multiarch 库目录拷贝（用 `-print-file-name` 解析真实路径，`-print-sysroot` 在部分发行版上不可靠）：

```bash
GLIBC_DIR=$(dirname $(aarch64-linux-gnu-gcc -print-file-name=libc.so.6))
cp -L $GLIBC_DIR/ld-linux-aarch64.so.1 rootfs/lib/
cp -L $GLIBC_DIR/libc.so.6 rootfs/lib/aarch64-linux-gnu/
cp -L $GLIBC_DIR/libgcc_s.so.1 rootfs/lib/aarch64-linux-gnu/
```

rbox 的动态链接依赖（`aarch64-linux-gnu-readelf -d` 确认）：
- NEEDED: libgcc_s.so.1
- NEEDED: libc.so.6
- Interpreter: /lib/ld-linux-aarch64.so.1
## 持久化 rootfs（ext4 磁盘镜像）

文件：src/applets/core/init/mod.rs（early_root_handoff）、Makefile（disk/run-disk）

除 initramfs 外，rbox 支持从持久 ext4 磁盘镜像启动：

```bash
make disk       # dd 建 64MB 镜像 + mkfs.ext4 -d rootfs 填充内容
make run-disk   # QEMU -drive virtio + root=/dev/vda
```

**启动流程**：

```
内核 ── initramfs(/init = rbox init) ── root=/dev/vda 检测
  ├─ 挂载 proc/sys/dev（initramfs 无这些目录，先创建）
  ├─ 解析 /proc/cmdline 的 root= → 挂载设备到 /newroot（显式 ext4）
  ├─ chdir(/newroot) + chroot(".") → 新根
  ├─ exec /init（新根上的 rbox init，PID 保持 1）
  └─ 新 init：statfs("/") 检测已是 ext4 → 跳过切换 → 正常初始化（挂 fstab/服务/getty）
```

- 用 **chroot** 而非 pivot_root/MS_MOVE：两者在 initramfs 的 rootfs 根上都返回
  EINVAL（内核限制）；chroot 无需挂载操作，代价是旧 initramfs 挂载树保留在
  内存中（约几 MB）。
- 二次切换防护：`statfs("/")` 的 f_type == EXT4_SUPER_MAGIC 时跳过
  （不能用 /proc/mounts 判断，chroot 后挂载表仍显示 rootfs）。
- 无 `root=` 内核参数时行为与原来完全一致（initramfs 模式）。

## 后续计划（剩余路线图）

> 历史计划中的 applet 扩展（dmesg/mount/umount/kill/find/chmod/chown）、shell 增强
> （test/[、算术、位置参数、read、复合命令、别名、$()、作业控制）、init 生产化
> （Before/EnvironmentFile/WorkingDirectory/TimeoutStopSec/KillMode、rescue 降级、
> logkeeper 轮转、UUID/LABEL 根设备）、工程化（musl 静态构建、fuzz-lite、覆盖率/
> 审计/发布目标）均已完成，见上文各章节与 CHANGELOG.md。以下是仍待实现的部分。

### 高优先级
| 项目 | 说明 |
|------|------|
| cgroup 进程跟踪 | forking 无 PIDFile 的 daemon 崩溃目前不触发 Restart；cgroup v2 可彻底解决 |
| Type=notify / WatchdogSec | sd_notify 协议与 per-service 健康检查（目前仅硬件看门狗） |
| socket activation | `.socket` 单元 + 按需拉起服务 |
| ExecStartPre/ExecStartPost | 启动前/后钩子（含超时与失败传播） |
| 启动失败降级细化 | rescue.target 独立单元、失败计数聚合、`systemctl` 风格 enable/disable |
| `xargs` / `sed` / `awk` | 脚本化最后三块常用工具（`sed`/`awk` 工作量大，可先做 `xargs`） |

### 中优先级
| 项目 | 说明 |
|------|------|
| 网络栈 | 内核 virtio-net + `ip`/`udhcpc`/`wget`/`ping`/`nc`（含 DNS 解析） |
| 时间同步 | NTP 客户端（或从 RTC 初始化系统时间） |
| udev/mdev | 设备节点动态管理（UUID 根设备依赖 /dev/disk/by-* 符号链接） |
| 多 target 切换 | boot/multi-user/rescue.target 与隔离语义 |
| fstab pass 字段 | 按 dump/pass 排序并 fsck（需 fsck 工具） |
| 日志结构化 | journald 风格索引/级别过滤；logkeeper 多文件轮转与压缩 |
| 终端 | 多 getty（多串口/虚拟终端）、utmp/wtmp/lastlog 记录 |
| 关机 | `shutdown -h +N` 定时、wall 广播、SIGPWR 处理、kexec |

### 低优先级 / 可选
| 项目 | 说明 |
|------|------|
| `case`/`until`/函数 | shell 脚本结构补全；`break N`/`continue N` |
| `$()` 词分割 | 命令替换按 POSIX 做词分割（当前不分割） |
| 密码策略 | 失败锁定/nologin/密码老化（目前仅失败延迟） |
| 服务隔离 | capabilities/no_new_privs/seccomp/只读根 |
| 供应链 | 内核 tarball 默认 sha256、SBOM、签名校验 |
| 多架构 | Makefile 参数化内核交叉编译（目前固定 aarch64） |

## 开发笔记

### Rust 2024 edition 注意事项

- `std::env::set_var` 在 edition 2024 中是 unsafe 的，需要 `unsafe { }` 包裹
- init.rs 和 shell.rs 中的 set_var 调用已正确处理

### 常见问题

**Q: 为什么用 libc crate 而不是直接 FFI？**
A: 早期版本使用直接 extern "C" FFI 声明系统调用，但存在类型安全、平台兼容性和维护性问题。现已全面改用 libc crate，统一管理所有系统调用：mount/sigaction/kill/sync/reboot/uname/time/localtime_r/utimensat 等。

**Q: 为什么用 glibc 而不是 musl？**
A: 用户选择 glibc 动态链接（aarch64-unknown-linux-gnu）。后续可以切换到 musl 静态链接以简化 rootfs。

**Q: 如何调试？**
A: QEMU 使用 -nographic 纯串口输出。可以通过 `-serial` 参数或 `-append "console=ttyAMA0"` 控制。也可以用 gdb 远程调试：在 QEMU 加 `-S -gdb tcp::1234`，用 aarch64-linux-gnu-gdb 连接。

**Q: 内核编译失败？**
A: 确保安装了 libelf-dev、flex、bison、bc、cpio、openssl。第一次编译可能需要 10-15 分钟。

**Q: 如何添加新的 TOML 服务单元？**
A: 在 rootfs/etc/rbox/system/ 下创建 .toml 文件，重新 `make initramfs` 打包即可。
