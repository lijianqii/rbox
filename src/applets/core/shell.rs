//! 基础 shell - 读取命令行、分词、fork+exec 传参。
//!
//! 功能范围：
//! - 读取一行输入，按空白分词，支持双引号、单引号、反斜杠转义。
//! - 反斜杠行尾续行（多行命令）。
//! - 注释 `#`（从 # 到行尾忽略）。
//! - 输出重定向：`>` 覆盖、`>>` 追加。
//! - 输入重定向：`<`。
//! - 管道：`|`（多级）。
//! - 控制操作符：`;`（顺序）、`&&`（成功后）、`||`（失败后）、`&`（后台）。
//! - 环境变量展开：`$VAR`、`${VAR}`、`$?`（上条退出码）、`$$`（PID）。
//! - 内置命令：`exit`、`cd`、`export`、`unset`、`pwd`、`echo`（回退内置）。
//! - 通配符展开：`*`、`?`、`[...]`（glob）。
//! - 命令查找：先按字面路径，否则在 PATH 下查找；再回退到 rbox 内置 applet。

use crate::applet::{self, Applet};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, ExitCode, Stdio};

/// 终端原始属性保存结构，shell 退出时恢复。
struct RawGuard {
    fd: i32,
    original: libc::termios,
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // 恢复原始终端属性
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

/// 将 fd 对应的终端设为 cbreak 模式（关闭 ICANON + ECHO），返回 guard。
/// 如果 fd 不是终端（如管道），返回 None，不影响读取。
fn enable_raw_mode() -> Option<RawGuard> {
    let fd = std::io::stdin().as_raw_fd();
    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        // 不是终端（管道/文件），跳过
        return None;
    }
    let mut raw = original;
    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, &raw);
    }
    Some(RawGuard { fd, original })
}

pub struct Shell;
pub static SHELL: &Shell = &Shell;

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    RedirOut,
    RedirAppend,
    RedirIn,
    Pipe,
    Semicolon,
    AndIf,
    OrIf,
    Background,
}

#[derive(Debug, Default)]
struct SimpleCmd {
    argv: Vec<String>,
    stdin_file: Option<String>,
    stdout_file: Option<String>,
    append: bool,
}

impl SimpleCmd {
    fn is_empty(&self) -> bool {
        self.argv.is_empty()
    }
}

struct Pipeline {
    cmds: Vec<SimpleCmd>,
    background: bool,
}

struct CommandList {
    segments: Vec<LogicalSegment>,
}

struct LogicalSegment {
    pipeline: Pipeline,
    connector: Connector,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Connector {
    Start,
    Sequential,
    AndIf,
    OrIf,
}

impl Applet for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn help(&self) -> &'static str {
        "shell - command interpreter (pipes, redirections, vars, glob)"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        // 进入 cbreak 模式：关闭行缓冲和回显，让我们逐字节读取按键。
        // _raw_guard 在函数退出时自动恢复终端。
        let _raw_guard = enable_raw_mode();

        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut last_rc: i32 = 0;
        let mut pending_line = String::new();

        // 命令历史
        let mut history: Vec<String> = Vec::new();
        let mut hist_idx: Option<usize> = None;  // None = 不在历史模式
        let mut saved_line = String::new();     // 进入历史前保存当前行

        let _ = write!(io::stdout(), "rbox# ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        let mut cursor: usize = 0;  // 光标字节偏移
        let mut byte_buf = [0u8; 1];

        loop {
            match input.read(&mut byte_buf) {
                Ok(0) => break,  // EOF
                Ok(_) => {
                    let b = byte_buf[0];
                    match b {
                        0x1b => {
                            // ESC 序列：读取后续字节（方向键、Home/End 等）
                            let mut seq = [0u8; 2];
                            if input.read(&mut seq[..1]).unwrap_or(0) == 1 && seq[0] == b'[' {
                                if input.read(&mut seq[1..2]).unwrap_or(0) == 1 {
                                    match seq[1] {
                                        b'A' => {
                                            // 上：上一条历史
                                            if !history.is_empty() {
                                                if hist_idx.is_none() {
                                                    saved_line = line.clone();
                                                    hist_idx = Some(history.len());
                                                }
                                                if let Some(idx) = hist_idx {
                                                    if idx > 0 {
                                                        hist_idx = Some(idx - 1);
                                                        line = history[idx - 1].clone();
                                                        cursor = line.len();
                                                        redraw(&pending_line, &line, cursor);
                                                    }
                                                }
                                            }
                                        }
                                        b'B' => {
                                            // 下：下一条历史
                                            if let Some(idx) = hist_idx {
                                                if idx + 1 < history.len() {
                                                    hist_idx = Some(idx + 1);
                                                    line = history[idx + 1].clone();
                                                } else {
                                                    // 回到保存的行
                                                    hist_idx = None;
                                                    line = saved_line.clone();
                                                }
                                                cursor = line.len();
                                                redraw(&pending_line, &line, cursor);
                                            }
                                        }
                                        b'C' => {
                                            // 右：光标右移
                                            if cursor < line.len() {
                                                // 移到下一个 UTF-8 字符边界
                                                let mut next = cursor + 1;
                                                while next < line.len()
                                                    && !line.is_char_boundary(next)
                                                {
                                                    next += 1;
                                                }
                                                cursor = next;
                                                let _ = write!(io::stdout(), "\x1b[C");
                                                let _ = io::stdout().flush();
                                            }
                                        }
                                        b'D' => {
                                            // 左：光标左移
                                            if cursor > 0 {
                                                // 移到上一个 UTF-8 字符边界
                                                let mut prev = cursor - 1;
                                                while prev > 0
                                                    && !line.is_char_boundary(prev)
                                                {
                                                    prev -= 1;
                                                }
                                                cursor = prev;
                                                let _ = write!(io::stdout(), "\x1b[D");
                                                let _ = io::stdout().flush();
                                            }
                                        }
                                        b'H' => {
                                            // Home：跳到行首
                                            cursor = 0;
                                            redraw(&pending_line, &line, cursor);
                                        }
                                        b'F' => {
                                            // End：跳到行末
                                            cursor = line.len();
                                            redraw(&pending_line, &line, cursor);
                                        }
                                        b'1' | b'7' => {
                                            // Home 的另一种编码：ESC[1~ 或 ESC[7~
                                            let mut dummy = [0u8; 1];
                                            let _ = input.read(&mut dummy);
                                            cursor = 0;
                                            redraw(&pending_line, &line, cursor);
                                        }
                                        b'4' | b'8' => {
                                            // End 的另一种编码：ESC[4~ 或 ESC[8~
                                            let mut dummy = [0u8; 1];
                                            let _ = input.read(&mut dummy);
                                            cursor = line.len();
                                            redraw(&pending_line, &line, cursor);
                                        }
                                        b'3' => {
                                            // Delete 键：ESC[3~
                                            let mut dummy = [0u8; 1];
                                            let _ = input.read(&mut dummy);
                                            if cursor < line.len() {
                                                // 删除光标处字符
                                                let mut end = cursor + 1;
                                                while end < line.len()
                                                    && !line.is_char_boundary(end)
                                                {
                                                    end += 1;
                                                }
                                                line.replace_range(cursor..end, "");
                                                redraw(&pending_line, &line, cursor);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        0x03 => {
                            // Ctrl-C：中断当前行，新起一行
                            let _ = writeln!(io::stdout(), "^C");
                            line.clear();
                            cursor = 0;
                            pending_line.clear();
                            hist_idx = None;
                            let _ = write!(io::stdout(), "rbox# ");
                            let _ = io::stdout().flush();
                        }
                        0x0c => {
                            // Ctrl-L：清屏并重绘当前行
                            let _ = write!(io::stdout(), "\x1b[2J\x1b[H");
                            redraw(&pending_line, &line, cursor);
                        }
                        0x01 => {
                            // Ctrl-A：跳到行首
                            cursor = 0;
                            redraw(&pending_line, &line, cursor);
                        }
                        0x05 => {
                            // Ctrl-E：跳到行末
                            cursor = line.len();
                            redraw(&pending_line, &line, cursor);
                        }
                        0x15 => {
                            // Ctrl-U：删除光标前的所有内容
                            if cursor > 0 {
                                line.drain(..cursor);
                                cursor = 0;
                                redraw(&pending_line, &line, cursor);
                            }
                        }
                        0x17 => {
                            // Ctrl-W：删除光标前一个单词
                            if cursor > 0 {
                                let mut i = cursor;
                                // 跳过空格
                                while i > 0 && line.as_bytes()[i - 1] == b' ' {
                                    i -= 1;
                                }
                                // 删除到前一个空格或行首
                                while i > 0 && line.as_bytes()[i - 1] != b' ' {
                                    i -= 1;
                                }
                                if i < cursor {
                                    line.drain(i..cursor);
                                    cursor = i;
                                    redraw(&pending_line, &line, cursor);
                                }
                            }
                        }
                        b'\t' => {
                            // Tab 补全
                            let (new_line, printed) = tab_complete(&line);
                            if new_line != line {
                                line = new_line;
                                cursor = line.len();
                                redraw(&pending_line, &line, cursor);
                            } else if printed {
                                redraw(&pending_line, &line, cursor);
                            }
                        }
                        b'\n' => {
                            // 回车：执行当前行
                            let _ = writeln!(io::stdout());
                            let mut full_line = line.clone();
                            line.clear();
                            cursor = 0;

                            // 续行检查
                            if full_line.ends_with('\\') && !full_line.ends_with("\\\\") {
                                pending_line = full_line.trim_end_matches('\\').to_string();
                                let _ = write!(io::stdout(), "> ");
                                let _ = io::stdout().flush();
                                continue;
                            } else if !pending_line.is_empty() {
                                // 拼接续行
                                full_line = format!("{}{}", pending_line, full_line);
                                pending_line.clear();
                            }

                            hist_idx = None;

                            // 执行行（历史扩展在 execute_line 内部完成）
                            // 注意：push 在 execute_line 之后，避免 !! 替换为当前行本身
                            last_rc = execute_line(&full_line, &mut last_rc, &history, |rc: i32| {
                                let _ = write!(io::stdout(), "rbox# ");
                                let _ = io::stdout().flush();
                                std::process::exit(rc);
                            });

                            // 存入历史（非空且与最后一条不同）
                            // 存原始行（未扩展），与 bash 行为一致
                            if !full_line.trim().is_empty() {
                                if history.last().map_or(true, |last| last != &full_line) {
                                    history.push(full_line.clone());
                                }
                            }

                            let _ = write!(io::stdout(), "rbox# ");
                            let _ = io::stdout().flush();
                        }
                        0x0b => {
                            // Ctrl-K：删除光标后的所有内容
                            if cursor < line.len() {
                                line.truncate(cursor);
                                redraw(&pending_line, &line, cursor);
                            }
                        }
                        0x7f | 0x08 => {
                            // 退格（DEL 或 BS）：删除光标前一个字符
                            if cursor > 0 {
                                // 找到前一个字符边界
                                let mut prev = cursor - 1;
                                while prev > 0 && !line.is_char_boundary(prev) {
                                    prev -= 1;
                                }
                                line.replace_range(prev..cursor, "");
                                cursor = prev;
                                redraw(&pending_line, &line, cursor);
                            }
                        }
                        0x04 => {
                            // Ctrl-D：空行时退出
                            if line.is_empty() {
                                let _ = writeln!(io::stdout());
                                break;
                            }
                        }
                        c if c >= 0x20 && c < 0x7f => {
                            // 可打印 ASCII：在光标处插入
                            line.insert(cursor, c as char);
                            cursor += 1;
                            redraw(&pending_line, &line, cursor);
                        }
                        _ => {
                            // 忽略其他控制字符
                        }
                    }
                }
                Err(_) => break,
            }
        }

        ExitCode::from(last_rc as u8)
    }
}

/// 重绘当前行：提示符 + pending_line + line，光标移到 cursor 位置。
fn redraw(pending_line: &str, line: &str, cursor: usize) {
    let prompt = if pending_line.is_empty() { "rbox# " } else { "> " };
    // \r 回到行首，\x1b[K 清除到行尾
    let _ = write!(io::stdout(), "\r\x1b[K{}{}{}", prompt, pending_line, line);
    // 将光标移到正确位置
    let display_pos = prompt.len() + pending_line.len() + cursor;
    // \r 回到行首，然后右移 display_pos 位
    let _ = write!(io::stdout(), "\r\x1b[{}C", display_pos);
    let _ = io::stdout().flush();
}

// ─── 行执行 ───────────────────────────────────────────

/// 执行一行命令。返回退出码。exit_code_fn 用于 exit 内置命令。
fn execute_line<F>(line: &str, last_rc: &mut i32, history: &[String], exit_fn: F) -> i32
where
    F: Fn(i32) -> (),
{
    // 历史扩展：!! -> 上一条命令，!n -> 第 n 条，!$ -> 上一条命令的最后一个参数
    let expanded_line = expand_history(line, history);
    let line = expanded_line.as_str();

    let tokens = tokenize(line);
    let cmd_list = match build_command_list(&tokens) {
        Ok(cl) => cl,
        Err(e) => {
            eprintln!("shell: {}", e);
            *last_rc = 2;
            return *last_rc;
        }
    };

    for seg in &cmd_list.segments {
        match seg.connector {
            Connector::Start | Connector::Sequential => {}
            Connector::AndIf => {
                if *last_rc != 0 {
                    continue;
                }
            }
            Connector::OrIf => {
                if *last_rc == 0 {
                    continue;
                }
            }
        }

        let expanded = match expand_pipeline(&seg.pipeline, *last_rc) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("shell: {}", e);
                *last_rc = 1;
                continue;
            }
        };

        if expanded.cmds.is_empty() {
            continue;
        }

        if expanded.cmds.len() == 1 && !expanded.background {
            match try_builtin(&expanded.cmds[0], last_rc, history) {
                BuiltinResult::Exit => {
                    exit_fn(*last_rc);
                }
                BuiltinResult::Done => continue,
                BuiltinResult::NotBuiltin => {}
            }
        }

        *last_rc = execute_pipeline(&expanded);
    }

    *last_rc
}

// ─── Tab 补全 ─────────────────────────────────────────

/// Tab 补全：根据当前行内容补全。
/// 返回 (补全后的完整行, 是否打印了多匹配列表)。
/// 如果在输入第一个词（命令名），补全命令。
/// 如果在输入后续词（参数），补全文件路径。
fn tab_complete(line: &str) -> (String, bool) {
    // 找到最后一个词的开始位置
    let word_start = find_last_word_start(line);
    let prefix = &line[word_start..];
    if prefix.is_empty() {
        return (line.to_string(), false);
    }

    // 判断是命令补全还是文件补全
    // 第一个词如果含 /（如 /bin/ls），走文件补全
    // 管道 | 后、分号 ; 后、&& / || 后也是新命令的开始，走命令补全
    let is_first_word = line[..word_start].trim().is_empty();
    let is_path = prefix.contains('/');
    let after_operator = {
        let before = line[..word_start].trim_end();
        before.ends_with('|') || before.ends_with(';')
            || before.ends_with("&&") || before.ends_with("||")
    };

    let matches = if (is_first_word || after_operator) && !is_path {
        complete_command(prefix)
    } else {
        complete_file(prefix)
    };

    if matches.is_empty() {
        return (line.to_string(), false);
    }

    if matches.len() == 1 {
        // 唯一匹配：补全整个词
        let completion = &matches[0];
        let mut new_line = line[..word_start].to_string();
        new_line.push_str(completion);
        // 目录补全后不加空格（用户可能要继续输入子路径），
        // 文件补全后加空格
        if !completion.ends_with('/') {
            new_line.push(' ');
        }
        (new_line, false)
    } else {
        // 多个匹配：找到公共前缀，补全到公共前缀
        let common = common_prefix(&matches);
        if common.len() > prefix.len() {
            // 有更多公共前缀可补全
            let mut new_line = line[..word_start].to_string();
            new_line.push_str(&common);
            (new_line, false)
        } else {
            // 无法继续补全，显示所有匹配（列格式排列）
            // 提取每个匹配的显示名（basename + 目录尾随 /）
            let displays: Vec<String> = matches.iter().map(|m| {
                let name = std::path::Path::new(m.trim_end_matches('/'))
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| m.clone());
                if m.ends_with('/') {
                    format!("{}/", name)
                } else {
                    name
                }
            }).collect();
            print_completions(&displays);
            (line.to_string(), true)
        }
    }
}

/// 计算字符串列表的公共前缀。
fn common_prefix(strs: &[String]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let first = &strs[0];
    let mut len = first.len();
    for s in &strs[1..] {
        len = len.min(s.len());
        let mut i = 0;
        while i < len && first.as_bytes()[i] == s.as_bytes()[i] {
            i += 1;
        }
        len = i;
        if len == 0 {
            break;
        }
    }
    first[..len].to_string()
}

/// 以列格式打印补全选项（类似 bash compgen）。
/// 自动计算列宽，按终端宽度（默认 80）排列为多列。
fn print_completions(items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!();

    // 计算最长项宽度
    let max_len = items.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    // 列宽 = 最长项 + 2 个空格间距
    let col_width = max_len + 2;
    // 终端宽度（rbox 环境通常为 80）
    let term_width = 80usize;
    let cols = (term_width / col_width).max(1);

    for (i, item) in items.iter().enumerate() {
        let pad = col_width - item.chars().count();
        print!("{}{}", item, " ".repeat(pad));
        // 换行条件：最后一列或最后一个元素
        if (i + 1) % cols == 0 || i == items.len() - 1 {
            println!();
        }
    }
}

/// 找到最后一个词的开始位置。
fn find_last_word_start(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = bytes.len();
    // 跳过尾部空白
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    // 找到词的开始
    while i > 0 {
        let c = bytes[i - 1];
        if c == b' ' || c == b'\t' || c == b'|' || c == b';' || c == b'&'
            || c == b'>' || c == b'<'
        {
            break;
        }
        i -= 1;
    }
    i
}

/// 命令补全：匹配内置 applet 名 + PATH 下的可执行文件。
fn complete_command(prefix: &str) -> Vec<String> {
    let mut matches = Vec::new();

    // 内置 applet
    for applet in applet::APPLETS {
        let name = applet.name();
        if name.starts_with(prefix) {
            matches.push(name.to_string());
        }
    }

    // 内置命令
    for builtin in &["cd", "exit", "export", "unset", "pwd"] {
        if builtin.starts_with(prefix) && !matches.iter().any(|m| m == builtin) {
            matches.push(builtin.to_string());
        }
    }

    // PATH 下的可执行文件
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(prefix) && !matches.iter().any(|m| m == &name) {
                        // 检查是否可执行
                        if let Ok(meta) = entry.metadata() {
                            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 != 0 {
                                matches.push(name);
                            }
                        }
                    }
                }
            }
        }
    }

    matches.sort();
    matches.dedup();
    matches
}

/// 文件补全：匹配文件系统路径。
fn complete_file(prefix: &str) -> Vec<String> {
    let path = std::path::Path::new(prefix);
    let (search_dir, file_prefix) = if prefix.ends_with('/') {
        (path.to_path_buf(), String::new())
    } else if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            (std::path::PathBuf::from("."), prefix.to_string())
        } else {
            (parent.to_path_buf(),
             path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default())
        }
    } else {
        (std::path::PathBuf::from("."), prefix.to_string())
    };

    let entries = match std::fs::read_dir(&search_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut matches: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&file_prefix) {
            // 如果是目录，加 /
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let display = if is_dir {
                format!("{}/", name)
            } else {
                name
            };
            // 返回完整路径（不含 search_dir 前缀如果 search_dir == ".")
            if search_dir == std::path::Path::new(".") && !prefix.contains('/') {
                matches.push(display);
            } else {
                let base = search_dir.to_string_lossy();
                // 避免 base 以 / 结尾时拼接出 //
                let full = if base.ends_with('/') {
                    format!("{}{}", base, display)
                } else {
                    format!("{}/{}", base, display)
                };
                matches.push(full);
            }
        }
    }

    matches.sort();
    matches.dedup();
    matches
}

fn tokenize(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_dquote = false;
    let mut in_squote = false;
    let mut in_token = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_squote {
            match c {
                '\'' => {
                    in_squote = false;
                    in_token = true;
                }
                _ => {
                    cur.push(c);
                    in_token = true;
                }
            }
            continue;
        }

        if in_dquote {
            match c {
                '"' => {
                    in_dquote = false;
                    in_token = true;
                }
                '\\' => {
                    if let Some(&next) = chars.peek() {
                        match next {
                            '$' | '`' | '"' | '\\' => {
                                chars.next();
                                cur.push(next);
                            }
                            '\n' => {
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        }
                    } else {
                        cur.push('\\');
                    }
                }
                _ => cur.push(c),
            }
            continue;
        }

        match c {
            '#' if !in_token => {
                break;
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    if next != '\n' {
                        cur.push(next);
                        in_token = true;
                    }
                }
            }
            '\'' => {
                in_squote = true;
                in_token = true;
            }
            '"' => {
                in_dquote = true;
                in_token = true;
            }
            '>' => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::RedirAppend);
                } else {
                    tokens.push(Token::RedirOut);
                }
            }
            '<' => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                tokens.push(Token::RedirIn);
            }
            '|' => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::OrIf);
                } else {
                    tokens.push(Token::Pipe);
                }
            }
            '&' => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::AndIf);
                } else {
                    tokens.push(Token::Background);
                }
            }
            ';' => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                tokens.push(Token::Semicolon);
            }
            ' ' | '\t' => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
            }
            _ => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(Token::Word(cur));
    }
    tokens
}

// ─── 命令列表构建 ─────────────────────────────────────

#[allow(unused_assignments)]
fn build_command_list(tokens: &[Token]) -> Result<CommandList, String> {
    let mut segments: Vec<LogicalSegment> = Vec::new();
    let mut cur_cmds: Vec<SimpleCmd> = Vec::new();
    let mut cur = SimpleCmd::default();
    let mut background = false;
    let mut connector = Connector::Start;
    let mut iter = tokens.iter().peekable();

    macro_rules! flush_pipeline {
        () => {{
            if !cur.is_empty() {
                cur_cmds.push(std::mem::take(&mut cur));
            }
            if !cur_cmds.is_empty() || background {
                segments.push(LogicalSegment {
                    pipeline: Pipeline {
                        cmds: std::mem::take(&mut cur_cmds),
                        background,
                    },
                    connector,
                });
                background = false;
                connector = Connector::Sequential;
            }
        }};
    }

    while let Some(tok) = iter.next() {
        match tok {
            Token::Word(w) => cur.argv.push(w.clone()),
            Token::RedirOut => {
                let f = next_word(&mut iter, ">")?;
                cur.stdout_file = Some(f);
                cur.append = false;
            }
            Token::RedirAppend => {
                let f = next_word(&mut iter, ">>")?;
                cur.stdout_file = Some(f);
                cur.append = true;
            }
            Token::RedirIn => {
                let f = next_word(&mut iter, "<")?;
                cur.stdin_file = Some(f);
            }
            Token::Pipe => {
                if cur.is_empty() {
                    return Err("syntax error: empty command before |".to_string());
                }
                cur_cmds.push(std::mem::take(&mut cur));
            }
            Token::Semicolon => {
                flush_pipeline!();
                connector = Connector::Sequential;
            }
            Token::AndIf => {
                if cur.is_empty() && cur_cmds.is_empty() {
                    return Err("syntax error: empty command before &&".to_string());
                }
                flush_pipeline!();
                connector = Connector::AndIf;
            }
            Token::OrIf => {
                if cur.is_empty() && cur_cmds.is_empty() {
                    return Err("syntax error: empty command before ||".to_string());
                }
                flush_pipeline!();
                connector = Connector::OrIf;
            }
            Token::Background => {
                if cur.is_empty() && cur_cmds.is_empty() {
                    return Err("syntax error: empty command before &".to_string());
                }
                flush_pipeline!();
                connector = Connector::Sequential;
            }
        }
    }
    flush_pipeline!();

    Ok(CommandList { segments })
}

fn next_word<'a, I>(iter: &mut std::iter::Peekable<I>, op: &str) -> Result<String, String>
where
    I: Iterator<Item = &'a Token>,
{
    match iter.next() {
        Some(Token::Word(w)) => Ok(w.clone()),
        _ => Err(format!("syntax error: expected filename after {}", op)),
    }
}

// ─── 变量展开 + glob ──────────────────────────────────

fn expand_pipeline(pipeline: &Pipeline, last_rc: i32) -> Result<Pipeline, String> {
    let mut new_cmds = Vec::with_capacity(pipeline.cmds.len());
    for cmd in &pipeline.cmds {
        let mut new_argv = Vec::with_capacity(cmd.argv.len());
        for arg in &cmd.argv {
            let expanded = expand_vars(arg, last_rc);
            // ~ 展开（仅对第一个参数或 = 后的路径展开）
            let expanded = expand_tilde(&expanded);
            let globs = expand_glob(&expanded);
            if globs.is_empty() {
                new_argv.push(expanded);
            } else {
                new_argv.extend(globs);
            }
        }
        new_cmds.push(SimpleCmd {
            argv: new_argv,
            stdin_file: cmd.stdin_file.clone(),
            stdout_file: cmd.stdout_file.clone(),
            append: cmd.append,
        });
    }
    Ok(Pipeline {
        cmds: new_cmds,
        background: pipeline.background,
    })
}

fn expand_vars(s: &str, last_rc: i32) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.peek() {
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc == '}' {
                            chars.next();
                            break;
                        }
                        name.push(nc);
                        chars.next();
                    }
                    result.push_str(&lookup_var(&name, last_rc));
                }
                Some('?') => {
                    chars.next();
                    result.push_str(&last_rc.to_string());
                }
                Some('$') => {
                    chars.next();
                    result.push_str(&std::process::id().to_string());
                }
                Some(&c2) if c2.is_ascii_alphabetic() || c2 == '_' => {
                    let mut name = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc.is_ascii_alphanumeric() || nc == '_' {
                            name.push(nc);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    result.push_str(&lookup_var(&name, last_rc));
                }
                _ => {
                    result.push('$');
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn lookup_var(name: &str, last_rc: i32) -> String {
    if name == "?" {
        return last_rc.to_string();
    }
    if name == "$" {
        return std::process::id().to_string();
    }
    std::env::var(name).unwrap_or_default()
}

/// 历史扩展：替换 !! !n !$ 等
/// !! -> 上一条命令
/// !n -> 第 n 条命令（1-based）
/// !-n -> 倒数第 n 条
/// !$ -> 上一条命令的最后一个参数
fn expand_history(line: &str, history: &[String]) -> String {
    if !line.contains('!') || history.is_empty() {
        return line.to_string();
    }

    let mut result = String::new();
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'!' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'!' => {
                    // !! -> 上一条命令
                    if let Some(last) = history.last() {
                        result.push_str(last);
                    }
                    i += 2;
                    continue;
                }
                b'$' => {
                    // !$ -> 上一条命令的最后一个参数
                    if let Some(last) = history.last() {
                        if let Some(arg) = last.split_whitespace().next_back() {
                            result.push_str(arg);
                        }
                    }
                    i += 2;
                    continue;
                }
                b'-' => {
                    // !-n -> 倒数第 n 条
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if end > start {
                        if let Ok(n) = std::str::from_utf8(&bytes[start..end]).unwrap().parse::<usize>() {
                            // 倒数第 n 条 = history[len - n]
                            if n > 0 && n <= history.len() {
                                let idx = history.len() - n;
                                result.push_str(&history[idx]);
                                i = end;
                                continue;
                            }
                        }
                    }
                }
                c if c.is_ascii_digit() => {
                    // !n -> 第 n 条命令（1-based）
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if let Ok(n) = std::str::from_utf8(&bytes[start..end]).unwrap().parse::<usize>() {
                        if n > 0 && n <= history.len() {
                            result.push_str(&history[n - 1]);
                            i = end;
                            continue;
                        }
                    }
                }
                _ => {}
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// ~ 展开：未引号包裹的 ~ 开头时展开为 $HOME
/// ~ -> $HOME
/// ~/path -> $HOME/path
/// ~user -> 用户 user 的 home（rbox 中仅支持 ~ 本身）
fn expand_tilde(s: &str) -> String {
    if s.starts_with('~') {
        // 检查 ~ 是否在引号内（简化检查：如果整个词被引号包裹则不展开）
        // ~ 或 ~/...
        if s == "~" || s.starts_with("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            if s == "~" {
                return home;
            } else {
                return format!("{}{}", home, &s[1..]);
            }
        }
    }
    s.to_string()
}

fn expand_glob(s: &str) -> Vec<String> {
    if !s.contains('*') && !s.contains('?') && !s.contains('[') {
        return Vec::new();
    }

    let dir = std::path::Path::new(s);
    let (search_dir, pattern) = if s.contains('/') {
        let parent = dir.parent().unwrap_or(std::path::Path::new("."));
        let fname = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (parent.to_path_buf(), fname)
    } else {
        (std::path::PathBuf::from("."), s.to_string())
    };

    let entries = match std::fs::read_dir(&search_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut matches: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !pattern.starts_with('.') {
            continue;
        }
        if glob_match(&pattern, &name) {
            if s.contains('/') {
                let full = search_dir.join(&name);
                matches.push(full.to_string_lossy().into_owned());
            } else {
                matches.push(name);
            }
        }
    }
    if !matches.is_empty() {
        matches.sort();
    }
    matches
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_inner(&p, &t)
}

fn glob_match_inner(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' => {
            // * 匹配任意数量字符（包括空）
            if p.len() == 1 {
                return true;
            }
            for i in 0..=t.len() {
                if glob_match_inner(&p[1..], &t[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => {
            if t.is_empty() {
                return false;
            }
            glob_match_inner(&p[1..], &t[1..])
        }
        '[' => {
            // 字符类: [abc] [a-z] [!abc]
            if t.is_empty() {
                return false;
            }
            let mut idx = 1;
            let mut negate = false;
            if idx < p.len() && p[idx] == '!' {
                negate = true;
                idx += 1;
            }
            let mut matched = false;
            while idx < p.len() && p[idx] != ']' {
                if idx + 2 < p.len() && p[idx + 1] == '-' && p[idx + 2] != ']' {
                    // 范围 [a-z]
                    if t[0] >= p[idx] && t[0] <= p[idx + 2] {
                        matched = true;
                    }
                    idx += 3;
                } else {
                    if t[0] == p[idx] {
                        matched = true;
                    }
                    idx += 1;
                }
            }
            // 找到 ]
            let rest = if idx < p.len() { &p[idx + 1..] } else { &p[idx..] };
            if matched != negate {
                glob_match_inner(rest, &t[1..])
            } else {
                false
            }
        }
        _ => {
            if t.is_empty() || p[0] != t[0] {
                return false;
            }
            glob_match_inner(&p[1..], &t[1..])
        }
    }
}

// ─── 内置命令 ─────────────────────────────────────────

/// 内置命令执行结果。
enum BuiltinResult {
    /// exit N：退出 shell
    Exit,
    /// 内置命令执行完成，继续下一行
    Done,
    /// 不是内置命令
    NotBuiltin,
}

/// 尝试执行内置命令。返回 BuiltinResult。
fn try_builtin(cmd: &SimpleCmd, last_rc: &mut i32, history: &[String]) -> BuiltinResult {
    if cmd.argv.is_empty() {
        return BuiltinResult::Done;
    }
    match cmd.argv[0].as_str() {
        "exit" => {
            let code = cmd.argv.get(1).and_then(|s| s.parse::<u8>().ok()).unwrap_or(*last_rc as u8);
            *last_rc = code as i32;
            return BuiltinResult::Exit;
        }
        "cd" => {
            let target = cmd.argv.get(1).cloned().unwrap_or_else(|| {
                std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
            });
            match std::env::set_current_dir(&target) {
                Ok(()) => {
                    *last_rc = 0;
                }
                Err(e) => {
                    eprintln!("cd: {}: {}", target, e);
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "pwd" => {
            match std::env::current_dir() {
                Ok(p) => println!("{}", p.display()),
                Err(e) => {
                    eprintln!("pwd: {}", e);
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "export" => {
            // export VAR=value  或  export VAR
            for arg in &cmd.argv[1..] {
                if let Some(eq) = arg.find('=') {
                    let (k, v) = arg.split_at(eq);
                    // SAFETY: single-threaded shell
                    unsafe { std::env::set_var(k, &v[1..]); }
                }
                // export VAR (已存在则标记，rbox 中等价于 noop)
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "unset" => {
            for arg in &cmd.argv[1..] {
                // SAFETY: single-threaded shell
                unsafe { std::env::remove_var(arg); }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "history" => {
            // 列出命令历史（带编号）
            for (i, h) in history.iter().enumerate() {
                println!("  {}  {}", i + 1, h);
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        _ => BuiltinResult::NotBuiltin,
    }
}

// ─── 执行器 ───────────────────────────────────────────

fn execute_pipeline(pipeline: &Pipeline) -> i32 {
    if pipeline.cmds.is_empty() {
        return 0;
    }

    let mut children: Vec<Child> = Vec::new();
    let ncmds = pipeline.cmds.len();

    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        if cmd.argv.is_empty() {
            continue;
        }

        let (program, extra_args) = resolve_command(&cmd.argv[0]);

        let mut command = Command::new(program);
        // 如果 resolve_command 返回了额外参数（如 rbox applet 名），先加它们
        command.args(&extra_args);
        // 再加原始命令的后续参数（跳过程序名本身）
        command.args(&cmd.argv[1..]);

        // stdin
        if let Some(ref f) = cmd.stdin_file {
            match std::fs::File::open(f) {
                Ok(file) => {
                    command.stdin(Stdio::from(file));
                }
                Err(e) => {
                    eprintln!("shell: {}: {}", f, e);
                    return 1;
                }
            }
        } else if i > 0 {
            // 管道中间命令：stdin 来自前一个命令的 pipe
            // 已在下面处理
        }

        // stdout
        if let Some(ref f) = cmd.stdout_file {
            let mut opts = std::fs::OpenOptions::new();
            let opts = if cmd.append {
                opts.create(true).append(true).open(f)
            } else {
                opts.create(true).write(true).truncate(true).open(f)
            };
            match opts {
                Ok(file) => {
                    command.stdout(Stdio::from(file));
                }
                Err(e) => {
                    eprintln!("shell: {}: {}", f, e);
                    return 1;
                }
            }
        } else if i < ncmds - 1 {
            // 管道：stdout piped 到下一个命令
            command.stdout(Stdio::piped());
        }

        // 如果是管道中间命令且不是第一个，stdin 从前一个的 stdout pipe 读取
        if i > 0 && cmd.stdin_file.is_none() {
            if let Some(prev) = children.last_mut() {
                if let Some(stdout) = prev.stdout.take() {
                    command.stdin(Stdio::from(stdout));
                }
            }
        }

        match command.spawn() {
            Ok(child) => {
                children.push(child);
            }
            Err(e) => {
                eprintln!("shell: {}: {}", cmd.argv[0], e);
                return 127;
            }
        }
    }

    // 后台运行：不等待
    if pipeline.background {
        return 0;
    }

    // 等待最后一个命令的退出码
    let mut last_code = 0;
    for child in &mut children {
        match child.wait() {
            Ok(status) => {
                last_code = status.code().unwrap_or(1);
            }
            Err(_) => {
                last_code = 1;
            }
        }
    }
    last_code
}

/// 命令查找：含 / 按字面路径，否则在 PATH 下查找。
/// 查找失败时回退到 rbox 内置 applet（rbox <cmd>）。
fn resolve_command(cmd: &str) -> (String, Vec<String>) {
    if cmd.contains('/') {
        return (cmd.to_string(), Vec::new());
    }

    // 在 PATH 下查找
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            let full = format!("{}/{}", dir, cmd);
            if std::path::Path::new(&full).is_file() {
                return (full, Vec::new());
            }
        }
    }

    // 回退：rbox 内置 applet。优先用当前可执行文件路径，
    // 其次 /bin/rbox（initramfs 环境下存在）。
    let rbox_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/bin/rbox".to_string());
    (rbox_path, vec![cmd.to_string()])
}
