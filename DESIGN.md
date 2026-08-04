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
| 二进制大小 | ~1.3MB（release） |
| initramfs 大小 | ~1.4MB |

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

内核源码放在 kernel/ 目录（Linux 6.12.36 LTS），使用 defconfig：

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
│   └── applets/            # 按功能分四组（core/file/text/sys）
│       ├── mod.rs          # 子模块声明
│       ├── core/           # 系统核心 applet + init 内部实现
│       │   ├── mod.rs      # 模块声明 + 共享 log()（kmsg/console）
│       │   ├── init/       # PID 1 实现（非 applet，仅 init 使用）
│       │   │   ├── mod.rs  # 入口：信号、run、主循环（回收/重启/关机）
│       │   │   ├── units.rs    # 单元 TOML 解析、单元名、拓扑排序
│       │   │   ├── services.rs # 服务生命周期：spawn/daemon化/重启退避/停止/降权
│       │   │   ├── server.rs   # 控制协议服务端（status/start/stop/restart/reload）
│       │   │   ├── mount.rs    # fstab 挂载、hostname、sysctl
│       │   │   └── syscall.rs  # libc 系统调用封装
│       │   ├── control.rs  # 控制协议客户端（status/rservice 共用）
│       │   ├── shell/       # 命令解释器（模块目录：mod/tokenizer/parser/expander/completion/builtin/executor/reader/types）
│       │   ├── shutdown.rs # shutdown（向 PID 1 发 SIGTERM）
│       │   ├── reboot.rs   # reboot（向 PID 1 发 SIGINT）
│       │   ├── status.rs   # status [unit]（unix socket 查询 init 服务状态）
│       │   └── rservice.rs # rservice（unix socket 管理 init 服务：start/stop/restart/reload）
│       ├── file/           # 文件与目录操作：ls、cp、mv、rm、mkdir、touch、ln、cat（+ util.rs 递归删除）
│       ├── text/           # 文本与字符串处理：head、tail、wc、grep、printf、echo、basename、dirname（+ util.rs 输入遍历）
│       └── sys/            # 系统信息与进程工具：true、false、pwd、uname、date、sleep、env
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

共 29 个 applet：

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
## Shell

文件：src/applets/core/shell/（模块目录，含单元测试）

一个命令解释器，REPL 循环逐字节读取输入并执行。提示符：`rbox# `（续行时 `> `）

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

### 实现细节

**输入模式**：逐字节读取 stdin，支持以下按键：

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

**命令历史**：`history: Vec<String>` 存储已执行命令（非空且与最后一条不同才入栈）。上键（\x1b[A）向上翻阅历史，下键（\x1b[B）向下翻阅，回到最新后恢复原始行。进入历史模式前保存当前行（`saved_line`），退出历史模式时恢复。

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

集成测试在 `tests/run_tests.sh` 中，通过 QEMU 全系统模拟运行所有命令。Shell 部分覆盖 10 个功能组、76 个断言：

| 测试组 | 测试项 | 数量 |
|--------|--------|------|
| 引号与转义 | 双引号、单引号、反斜杠、续行、注释 | 5 |
| 变量展开 | $VAR、${VAR}、$?、$$、unset | 5 |
| 控制操作符 | ;、&&、||、链式、后台 & | 5 |
| 重定向 | >、>>、< | 3 |
| 管道 | 3级管道、管道+重定向 | 3 |
| 通配符 | *、?、[] | 4 |
| 历史扩展 | !!、!n、!$、history | 4 |
| ~ 展开 | echo ~、cd ~ + pwd | 2 |
| Tab 补全 | 命令、文件、管道后 | 3 |
| 行编辑快捷键 | Ctrl-E/U/K/W/C | 5 |

> **注意**：Ctrl-A (0x01) 在 QEMU `-nographic` 模式下是 monitor 转义前缀，不会传递给客户机，因此无法在自动化测试中覆盖。Ctrl-A 在交互式 `make run` 中可正常使用（宿主机 stty raw 模式下传递）。

**已知限制**：
- Ctrl-A 被 QEMU `-nographic` 截获，自动化测试无法覆盖
- Shell 不支持 `for`/`while`/`if` 等复合命令
- 不支持命令别名 `alias`
- 不支持子shell `()` 和命令替换 `$()`

### 终端模式（Tab 补全的前提）

Tab 补全要求 shell 能逐字节读取按键，但默认终端处于 **canonical（行缓冲）模式**，按下 Tab 不会立即传递给进程。因此需要两层终端设置：

1. **客户机侧（shell 启动时）**：`enable_raw_mode()` 通过 `libc::tcgetattr` 保存原始终端属性，`tcsetattr` 设置 cbreak 模式（关闭 `ICANON` + `ECHO`，`VMIN=1 VTIME=0`）。`RawGuard` 在 shell 退出时通过 `Drop` 自动恢复。管道输入时 `tcgetattr` 失败返回 `None`，不影响。
2. **宿主机侧（make run / run.sh）**：`stty -echo -icanon min 1 time 0` 将宿主机终端设为 raw 模式，让按键立即传递给 QEMU。QEMU 退出后 `stty sane` 恢复。

```rust
struct RawGuard { fd: i32, original: libc::termios }
impl Drop for RawGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original); }
    }
}
```

文件：src/applets/core/init.rs（含单元测试）

一个 systemd 风格的 PID 1 初始化进程，使用 TOML 格式的单元文件配置。

### 配置文件

单元文件放在 `/etc/rbox/system/*.toml`，使用 systemd 风格的三段式结构：

```toml
# /etc/rbox/system/hello.service.toml

[Unit]
Description = "Hello service"
Name = "hello"                        # 可选：单元名（rservice/status/依赖引用用它；缺省回退文件名）
After = ["network.service"]        # 可选：在此服务之后启动
Requires = ["network.service"]     # 可选：硬依赖

[Service]
Type = "simple"                    # simple（默认）/ forking（daemon 化）
ExecStart = "/bin/rbox echo hello" # 启动命令
ExecStop = "/bin/rbox echo bye"    # 可选：关机时执行的停止命令
ExecReload = "/bin/rbox echo ok"   # 可选：rservice reload 执行的命令
Environment = ["HELLO=world"]      # 可选：服务环境变量
Restart = "on-failure"             # 可选：非零退出自动重启（默认 no）
RestartSec = 1                      # 可选：重启间隔秒（默认 1）
StartLimitBurst = 5                 # 可选：连续失败上限（默认 5，达到后放弃）
TimeoutStartSec = 10                # 可选：forking 等待父进程退出超时（默认 10）
PIDFile = "/var/run/x.pid"         # 可选：forking 的 daemon PID 文件
LogFile = "/var/log/x.log"         # 可选：stdout/stderr 重定向文件
User = "nobody"                    # 可选：降权用户（getpwnam）
Group = "nogroup"                  # 可选：降权组（getgrnam）
Console = true                     # 可选：前台 console 服务（如交互 shell，退出自动 respawn）

[Install]
WantedBy = ["default.target"]      # 被哪个 target 拉入
```

target 文件（如 default.target.toml）本身不含 ExecStart，仅作为依赖图的根节点。

**单元命名**：`[Unit] Name = "..."` 显式声明单元名（rservice/status/依赖引用均使用它）；缺省时回退文件名（去掉 `.toml`）。target 类型按文件名 `.target` 后缀判定，不受 Name 影响。

### 启动流程

1. **信号处理**：安装 SIGTERM/SIGINT 处理器（SIGTERM 设关机标志，SIGINT 设重启标志）
2. **环境与挂载**：设置默认 PATH（shell/服务子进程继承）；读取 /etc/fstab 逐个挂载（缺失时回退内置默认集：proc/sysfs/devtmpfs/devpts/tmpfs）；读取 /etc/hostname 设置主机名（sethostname）
3. **加载单元**：解析 /etc/rbox/system/*.toml，serde 反序列化
4. **拓扑排序**：从 default.target 出发 DFS，Requires=/After= 构成边，WantedBy= 构成反向依赖（target 拉入所有 WantedBy 它的服务），含环检测
5. **启动服务**：按排序结果依次 fork+exec ExecStart（独立进程组，带 Environment），记录 Child 句柄和 ExecStop；`Console = true` 的服务作为前台 console 等待
6. **常驻**：主循环回收 console shell（退出则 respawn）与服务进程（try_wait，避免僵尸）；`Restart=on-failure` 的服务非零退出后自动重新拉起；**waitpid(-1) 收割收养的孤儿进程**（防僵尸累积）；通过 `/tmp/rbox.sock` 响应控制请求（`status`/`start`/`stop`/`restart`，供 rbox status / rservice 使用）；检测关机标志

`Type=` 目前仅支持 `simple`，遇到其他值会打印警告并按 simple 处理。
`Restart=` 目前仅支持 `no`（默认）与 `on-failure`，其他值打印警告并按 no 处理。

关机时按进程组（`process_group(0)`）SIGTERM 服务及其后代进程，1 秒超时后 SIGKILL，不再只杀直接子进程。

### 控制协议（/tmp/rbox.sock）

单行请求，文本响应：

| 请求 | 说明 |
|------|------|
| `status` / 空 | 列出全部服务状态（init、console、各服务） |
| `status <unit>` | 查询单个单元 |
| `start <unit>` | 启动服务（已停止的重新拉起；未启动过的从单元文件新建） |
| `stop <unit>` | 停止服务（执行 ExecStop + SIGTERM 进程组，超时 SIGKILL；标记 stopped 禁止自动重启） |
| `restart <unit>` | 停止后重新启动 |
| `reload <unit>` | 执行 ExecReload 命令（不重启进程） |

客户端：`rbox status` / `rservice`（list/status/start/stop/restart/reload）。console 服务由 init 独占管理，不接受 stop/restart。

### 关机/重启流程

```
shutdown 命令 / SIGTERM ──► 关机（设置 SHUTDOWN_REQUESTED 标志）
reboot 命令   / SIGINT  ──► 重启（设置 REBOOT_REQUESTED 标志）
  |
  +-- 设置对应全局标志
  +-- 主循环检测到标志 -> kill console shell
  +-- do_shutdown():
  |     +-- 逆序遍历已启动的服务，执行 ExecStop + SIGTERM 等服务退出
  |     |    （1 秒超时后 SIGKILL 强杀，避免挂起）
  |     +-- kill(-1, SIGTERM) -> 所有残留进程
  |     +-- sleep 500ms
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

libc::reboot 使用 glibc 封装的简化签名 `reboot(how_to)`，不需要手动传递 magic number。

### 当前 TOML 单元文件（生产 rootfs）

| 文件 | 类型 | Name | 说明 |
|------|------|------|------|
| default.target.toml | target | default.target | 启动根节点（无 Name 字段，回退文件名） |
| console-shell.service.toml | service | console-shell | ExecStart=/bin/rbox shell，Console=true 前台交互 shell |

测试专用服务（hello、restart-test、longrun、forktest、forktimeout、usertest）放在 `tests/units/`，由集成测试脚本运行时注入 rootfs 并打包独立的测试 initramfs，测试结束自动清理，不进入生产镜像。

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
| make rootfs | 拷贝 rbox 二进制 + 创建 29 个 applet 符号链接 + 拷贝 glibc 运行时 |
| make initramfs | 将 rootfs/ 打包为 initramfs.cpio.gz（newc 格式 + gzip） |
| make run | QEMU 全系统模拟启动 |
| make test | 运行集成测试 |
| make kernel | 编译 ARM64 内核（defconfig + Image） |
| make clean | 清理产物 |
| make help | 显示帮助 |

### QEMU 启动参数

```bash
qemu-system-aarch64 \
    -M virt \
    -cpu cortex-a72 \
    -m 512M \
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

| 类别 | 测试项 | 数量 |
|------|--------|------|
| 基本 applet | uname -m、uname -n（主机名）、pwd、echo、cat | 5 |
| 文件操作 | 重定向写入、cp、ls | 3 |
| 管道与重定向 | 管道 cat\|cat、追加写入 | 3 |
| init 启动流程 | PID 1 启动、fstab 挂载、加载单元、reached target | 5 |
| 服务管理 | Environment 注入、Restart 自动重启、status 查询×4 | 6 |
| rservice 管理 | stop、start、restart、list | 4 |
| init 增强 | ExecReload、console reload、status 单查 console、sysctl、User= 降权、forking 等待、forking 超时、kmsg 日志 | 8 |
| Shell 增强 | export + 变量展开、命令分隔 ;、条件执行 &&、条件执行 \|\|、上下键历史、Ctrl-A/E/U/W/L | 6 |
| Tab 补全 | 命令补全 ec->echo、文件补全 /etc/host->hostname、管道后命令补全 | 3 |
| 文本处理 applets | head -n、printf %s/%d、wc -c、grep -o、basename、basename 去后缀、dirname、date、env | 9 |
| 文件 applets | ln -s 创建+读取、echo -n、ls -a、ls -1、rm -r、touch、mkdir -p | 7 |
| tail | tail -n 1 末尾行 | 1 |
| 行编辑快捷键 | Ctrl-E、Ctrl-U、Ctrl-K、Ctrl-W、Ctrl-C | 5 |
| 关机流程 | shutdown 触发、ExecStop 逆序、power off | 3 |
| **合计** | | **68** |

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
| shell/tokenizer | tokenize（引号/转义/重定向/管道/控制操作符/注释/续行） | 13 |
| shell/parser | parse（逻辑段/语法错误/后台执行/管道） | 13 |
| shell/expander | expand_vars（$VAR/${VAR}/$?/$$）、expand_history（!!/!n/!-n）、expand_tilde、expand_glob（* ? []） | 21 |
| shell/completion | find_last_word_start、complete_command、complete_file（根路径/嵌套路径/尾斜杠）、common_prefix | 20 |
| init/units | parse_cmdline、compute_start_order、parse_fstab、parse_mount_flags、parse_environment、format_status、parse_control_request | 24 |
| text/basename | basename（路径/后缀/根/尾随斜杠） | 7 |
| text/dirname | dirname（路径/根/无目录/尾随斜杠） | 5 |
| text/printf | printf_format（%s/%d/%x/%c/%%/转义序列） | 12 |
| text/echo | echo_format（简单/-n/多参数/空参数/-n仅自身/第二个-n为文本） | 7 |
| text/grep | grep_search（基本/忽略大小写/反向/行号/文件名前缀）、parse_grep_args（基本/缺模式/组合标志） | 11 |
| text/head | head_lines（基本/超出范围/零行/空输入/无尾随换行/单行） | 6 |
| text/tail | tail_lines（基本/超出范围/零行/空输入/单行/精确计数） | 6 |
| text/wc | WcCounts（行/字/字节计数） | 4 |
| text/util | parse_n_files（默认值/显式/-nN/多文件） | 4 |
| file/util | remove_recursive、is_dir、resolve_dest、copy_recursive | 7 |
| file/ls | mode_string_from_mode（普通/可执行/目录/符号链接/无权限）、filter_entries（隐藏/显示全部/空） | 8 |
| **合计** | | **168** |

测试结果示例：

```
========================================

[基本 applet]          5/5 PASS
[文件操作]             3/3 PASS
[管道与重定向]          3/3 PASS
[init 启动流程]        5/5 PASS
[服务管理]             6/6 PASS
[rservice 管理]        4/4 PASS
[init 增强]            8/8 PASS
[Shell 增强]           6/6 PASS
[Tab 补全]             3/3 PASS
[文本处理 applets]     9/9 PASS
[文件 applets]         7/7 PASS
[tail]                 1/1 PASS
[行编辑快捷键]          5/5 PASS
[关机流程]             3/3 PASS

========================================
结果: 68 通过, 0 失败
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
│       └── libgcc_s.so.1        # GCC 运行时
└── etc/
    ├── hostname                 # 主机名（init 启动时读取）
    ├── fstab                    # init 挂载表
    └── rbox/
        └── system/              # init TOML 单元文件（生产：仅 default.target + console-shell）
            ├── default.target.toml
            └── console-shell.service.toml
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
## 后续计划

按优先级排列：

### 第一优先级：扩展 Applet

这是当前阶段的主要工作。以下为建议的 applet 及优先级：

**高优先级（核心工具）**：

| Applet | 用法 | 状态 |
|--------|------|------|
| dmesg | dmesg | 未实现 |
| mount | mount [-t type] src tgt | 未实现 |
| umount | umount tgt | 未实现 |
| ps | ps | 未实现 |
| kill | kill [-signal] pid | 未实现 |
| find | find path [-name pattern] | 未实现 |
| chmod | chmod mode file | 未实现 |
| chown | chown user:group file | 未实现 |

**已实现（第一批扩展）**：

| Applet | 用法 |
|--------|------|
| head | head [-n N] [file] |
| tail | tail [-n N] [file] |
| wc | wc [-lwc] [file] |
| grep | grep [-inv] PATTERN [file] |
| ln | ln [-s] target link |
| date | date |
| sleep | sleep N |
| env | env [name=val] [cmd] |
| printf | printf format args... |
| basename | basename path [suffix] |
| dirname | dirname path |

**中优先级（实用工具）**：

| Applet | 用法 | 状态 |
|--------|------|------|
| tar | tar [xf\|cf] file | 未实现 |
| dd | dd if= of= bs= | 未实现 |
| du | du | 未实现 |
| stat | stat file | 未实现 |
| sort | sort [file] | 未实现 |
| uniq | uniq | 未实现 |
| cut | cut -d -f | 未实现 |
| tr | tr set1 set2 | 未实现 |
| test | test expr | 未实现 |
| xargs | xargs cmd | 未实现 |

### 第二优先级：Shell 增强

| 功能 | 说明 |
|------|------|
| 环境变量 $VAR | 支持变量展开和赋值 VAR=value |
| 命令分隔 ; | 顺序执行多条命令 |
| 条件执行 && / \|\| | 根据退出码决定是否执行 |
| 后台运行 & | fork 子进程后台执行 |
| 通配符 * ? | glob 展开 |
| 退出码 $? | 上条命令退出码 |
| 命令历史 | readline 式输入 |
| if/for/while | 控制结构 |

### 第三优先级：Init 增强

以下功能已在后续迭代中实现：

| 功能 | 说明 | 状态 |
|------|------|------|
| Restart=on-failure | 服务退出后自动重启 | ✅ 已实现 |
| RestartSec / StartLimitBurst | 重启退避间隔与连续失败上限（防 crash-loop 刷屏） | ✅ 已实现 |
| Type=forking | daemon 化服务：等待父进程退出 + PIDFile 跟踪 + TimeoutStartSec 超时 | ✅ 已实现 |
| ExecReload | rservice reload <unit> 执行 ExecReload 命令（不重启） | ✅ 已实现 |
| 服务输出重定向 | LogFile= 将 stdout/stderr 写入日志文件 | ✅ 已实现 |
| User=/Group= 降权 | 以指定用户/组运行（getpwnam/getgrnam 解析） | ✅ 已实现 |
| sysctl 支持 | 启动时应用 /etc/sysctl.conf（写 /proc/sys/*） | ✅ 已实现 |
| 日志写 /dev/kmsg | init 日志进入内核环形缓冲（dmesg/console 回显可见） | ✅ 已实现 |
| Environment= | 服务环境变量 | ✅ 已实现 |
| 前台/后台服务区分 | Console=true 显式标记 | ✅ 已实现 |
| 服务状态查询 | rbox status / status <unit>（unix socket） | ✅ 已实现 |
| 服务管理命令 | rservice start/stop/restart/reload（unix socket 控制协议） | ✅ 已实现 |
| 进程组清理 | 服务独立进程组，关机按组终止后代 | ✅ 已实现 |
| 多 target 切换 | boot.target / multi-user.target / rescue.target | TODO |
| 依赖更精细控制 | Wants= / Requisite= / Before= | TODO |
| ExecStartPre/Post 钩子 | 启动前/后执行额外命令 | TODO |
| 内核 cmdline 解析 | single/emergency（跳过服务直接进 shell）、quiet | TODO |
| 启动失败降级 | default.target 失败 → 自动进入 rescue | TODO |
| 看门狗喂狗 | /dev/watchdog 周期性喂狗，挂死自动重启 | TODO |
| 静态网络配置 | [Network] Address=/Gateway= 设置 IP | TODO |
| SIGCHLD 驱动回收 | 信号触发立即 try_wait，替代 200ms 轮询 | TODO |
| ExecStop 超时 | ExecStop 命令超时限制 | TODO |
| fstab pass 字段 | 按 dump/pass 决定挂载顺序 | TODO |
| head 字符设备兼容 | head 读取 /dev/kmsg 等设备文件（当前 EINVAL） | TODO |

### 第四优先级：工程化进阶

| 功能 | 说明 | 状态 |
|------|------|------|
| CI 流水线 | GitHub Actions 自动构建 + 测试 | 不需要 |
| 单元测试 | Rust #[test] 模块（168 个） | ✅ 已实现 |
| Clippy 零警告 | 全量修复 clippy warning | ✅ 已实现 |
| rustfmt 统一格式 | rustfmt.toml 配置 | ✅ 已实现 |
| Makefile verify 目标 | check + clippy + unittest 一键验证 | ✅ 已实现 |
| Makefile APPLETS 自动同步 | 从 cargo run --list 提取 applet 列表 | ✅ 已实现 |
| 共享工具提取 | file/util.rs（remove_recursive/is_dir/resolve_dest/copy_recursive） | ✅ 已实现 |
| applet --help 支持 | `rbox <applet> --help` 打印帮助 | ✅ 已实现 |
| 静态链接 musl | 减小 rootfs 依赖（aarch64-unknown-linux-musl） | TODO |
| 压缩二进制 | UPX 或 strip 减小体积 | TODO |
| 持久化根文件系统 | ext4 磁盘镜像 + 真正的 init（非 initramfs） | TODO |
| 网络支持 | 内核配置 virtio-net + busybox-style 网络工具 | TODO |

---

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
