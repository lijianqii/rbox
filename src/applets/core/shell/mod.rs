//! 基础 shell - 读取命令行、分词、fork+exec 传参。
//!
//! 功能范围：
//! - 读取一行输入，按空白分词，支持双引号、单引号、反斜杠转义。
//! - 反斜杠行尾续行（多行命令）。
//! - 注释 `#`（从 # 到行尾忽略）。
//! - 多级管道 `cmd1 | cmd2 | cmd3`。
//! - 输出重定向 `>` `>>`、输入重定向 `<`。
//! - 内置命令 `cd` `exit` `export` `unset` `pwd` `history`。
//! - 环境变量展开 `$VAR` `${VAR}` `$?` `$$`。
//! - 控制操作符 `;` `&&` `||` `&`。
//! - 通配符 `*` `?` `[]`。
//! - Tab 补全（命令 + 文件）。
//! - 命令历史（上/下键）+ 历史扩展 `!!` `!n` `!$`。
//! - 行编辑快捷键（Ctrl-A/E/U/K/W/C/L、Home/End/Delete）。
//! - Ctrl-C SIGINT 转发：中断前台运行命令而不退出 shell。
//! - `~` 展开。

mod alias;
mod builtin;
mod completion;
mod compound;
mod executor;
mod expander;
mod functions;
#[cfg(test)]
mod fuzz;
mod jobs;
mod options;
mod params;
mod parser;
mod reader;
mod script;
mod tokenizer;
mod trap;
mod types;

use crate::applet::Applet;
use reader::{enable_raw_mode, make_continuation_prompt, make_prompt, redraw};
use std::io::{self, Read, Write};

/// 无缓冲 stdin：直接 `read(2)`。Rust 的 `Stdin` 会预读缓冲，导致 `read`
/// 内置命令与 REPL 争抢输入（缓冲吃掉后续行）；REPL 改用裸 fd 读取后，
/// 两者共享同一文件偏移，行为与 POSIX 一致。
struct RawStdin;

impl Read for RawStdin {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

/// 从 `Read` 读一行（含换行符；EOF 返回已读内容）。
fn read_line_raw<R: Read>(input: &mut R) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match input.read(&mut b) {
            Ok(0) => break,
            Ok(_) => {
                buf.push(b[0]);
                if b[0] == b'\n' {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    buf
}

/// 处理 here-doc：检测 `<<DELIM`，读取后续行直到 DELIM，写入临时文件。
/// 返回替换后的命令行（`<<DELIM` -> `<tmpfile`）。
fn process_heredoc<R: Read>(line: &str, input: &mut R) -> String {
    // 查找真正的 <<（跳过引号内与反斜杠转义）
    let idx = match find_heredoc_operator(line) {
        Some(i) => i,
        None => return line.to_string(),
    };

    // 解析 delimiter：支持 `<<-`（去 tab）与引号 delimiter（不展开）
    let after = line[idx + 2..].trim_start();
    let (strip_tabs, delim_raw) = match after.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, after),
    };
    let delim = delim_raw.split_whitespace().next().unwrap_or("");
    if delim.is_empty() {
        return line.to_string();
    }
    let quoted = (delim.starts_with('\'') && delim.ends_with('\''))
        || (delim.starts_with('"') && delim.ends_with('"'));
    let delim_clean = delim.trim_matches(|c| c == '\'' || c == '"');

    // 读取 here-doc 内容（按行读取，保留 UTF-8）
    let mut content = String::new();
    loop {
        let _ = write!(io::stdout(), "{}", make_continuation_prompt());
        let _ = io::stdout().flush();
        let mut line_buf = read_line_raw(input);
        if line_buf.is_empty() {
            break; // EOF
        }
        // 去掉行尾换行符（\n 或 \r\n）
        if line_buf.last() == Some(&b'\n') {
            line_buf.pop();
            if line_buf.last() == Some(&b'\r') {
                line_buf.pop();
            }
        }
        let text = String::from_utf8_lossy(&line_buf).into_owned();
        let candidate = if strip_tabs {
            text.trim_start_matches('\t').to_string()
        } else {
            text
        };
        if candidate == delim_clean {
            break;
        }
        content.push_str(&candidate);
        content.push('\n');
    }

    // 未加引号的 delimiter 展开变量与命令替换
    let expanded = script::expand_heredoc_body(&content, !quoted);

    // 写入临时文件（序号避免同进程内多次 heredoc 冲突）
    let seq = HEREDOC_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tmpfile = format!("/tmp/rbox_heredoc_{}_{}", std::process::id(), seq);
    if std::fs::write(&tmpfile, &expanded).is_err() {
        return line.to_string();
    }

    // 替换 <<DELIM 为 <tmpfile
    let before = &line[..idx];
    format!("{} < {}", before.trim_end(), tmpfile)
}

/// here-doc 临时文件序号。
static HEREDOC_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 读取一个完整 UTF-8 字符（首字节已读入为 `first`），返回该字符的字符串形式。
/// 多字节序列按首字节判断长度；无效/不完整序列按已读字节用 U+FFFD 替换。
fn read_utf8_char<R: Read>(first: u8, input: &mut R) -> String {
    let extra = match first {
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        _ => 0,
    };
    let mut buf = vec![first];
    for _ in 0..extra {
        let mut b = [0u8; 1];
        if input.read(&mut b).unwrap_or(0) != 1 {
            break;
        }
        buf.push(b[0]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// 查找行中真正的 `<<` 操作符位置（跳过单/双引号内与反斜杠转义）。
pub(crate) fn find_heredoc_operator(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_squote {
            if b == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match b {
                b'"' => in_dquote = false,
                b'\\' => i += 1, // 跳过转义字符
                _ => {}
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'\\' => i += 1, // 跳过转义字符
            // `<<` 是 here-doc；`<<<` 是 here-string（不收集后续行）
            b'<' if (i == 0 || bytes[i - 1] != b'<')
                && i + 2 < bytes.len()
                && bytes[i + 1] == b'<'
                && bytes[i + 2] != b'<' =>
            {
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 判断行是否以"续行反斜杠"结尾（单引号内反斜杠不续行；双引号/引号外的
/// 行尾反斜杠续行，被转义的反斜杠不续行）。
pub(crate) fn needs_continuation(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_squote {
            if b == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match b {
                b'"' => in_dquote = false,
                b'\\' => {
                    if i + 1 >= bytes.len() {
                        return true; // 双引号内行尾反斜杠 = 续行
                    }
                    i += 1; // 跳过转义字符
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'\\' => {
                if i + 1 >= bytes.len() {
                    return true; // 行尾反斜杠 = 续行
                }
                i += 1; // 跳过转义字符
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// 中断当前行输入（Ctrl-C / EINTR）：清空行与续行缓冲，打印 ^C 并重绘提示符。
fn abort_line(
    line: &mut String,
    cursor: &mut usize,
    pending_line: &mut String,
    hist_idx: &mut Option<usize>,
) {
    let _ = writeln!(io::stdout(), "^C");
    line.clear();
    *cursor = 0;
    pending_line.clear();
    *hist_idx = None;
    let _ = write!(io::stdout(), "{}", make_prompt(pending_line));
    let _ = io::stdout().flush();
}

/// 清空历史请求（`history -c` 由内置设置，REPL 消费）。
static HISTORY_CLEAR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 请求清空历史。
pub(crate) fn mark_history_clear() {
    HISTORY_CLEAR.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// 消费清空历史请求。
pub(crate) fn take_history_clear() -> bool {
    HISTORY_CLEAR.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// 历史文件路径：`$HISTFILE` > 配置 [paths] history_file > `$HOME/.rbox_history`。
fn history_file() -> String {
    if let Ok(hf) = std::env::var("HISTFILE")
        && !hf.is_empty()
    {
        return hf;
    }
    let configured = &crate::config::load().paths.history_file;
    if !configured.is_empty() {
        return expand_tilde_path(configured);
    }
    std::env::var("HOME")
        .map(|h| format!("{}/.rbox_history", h))
        .unwrap_or_else(|_| "/tmp/.rbox_history".to_string())
}

/// 展开路径中的 `~` 前缀为 $HOME（未设置时保持原样）。
fn expand_tilde_path(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!(
            "{}/{}",
            std::env::var("HOME").unwrap_or_else(|_| "".to_string()),
            rest
        );
    }
    path.to_string()
}

/// 加载历史文件。
fn load_history() -> Vec<String> {
    let path = history_file();
    let size = std::env::var("HISTSIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(500);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
            if lines.len() > size {
                lines.drain(..lines.len() - size);
            }
            lines
        }
        Err(_) => Vec::new(),
    }
}

/// 追加一条历史到文件。
fn append_history(line: &str) {
    let path = history_file();
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// 执行 source 命令：逐行读取文件并执行。
fn source_file(path: &str, last_rc: &mut i32, history: &mut [String]) -> i32 {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("source: {}: {}", path, e);
            return 1;
        }
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        *last_rc = executor::execute_line(line, last_rc, history, &|_rc: i32| {
            // source 中不支持 exit
        });
        // 注意：source 的行不应进入交互式历史（与 bash 一致），
        // 否则 /etc/profile 等启动脚本会污染 `history` 输出与 `!n` 历史索引。
    }
    *last_rc
}

pub struct Shell;

pub static SHELL: &Shell = &Shell;

impl Applet for Shell {
    fn name(&self) -> &'static str {
        "sh"
    }

    fn help(&self) -> &'static str {
        "rbox shell - minimalist interactive shell"
    }

    fn run(&self, args: &[String]) -> std::process::ExitCode {
        let parsed = match script::parse_shell_args(args) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("sh: {}", e);
                return std::process::ExitCode::from(2);
            }
        };
        if let Some(cmd) = parsed.command {
            return std::process::ExitCode::from(
                (script::run_command_string(&cmd, parsed.args) & 0xff) as u8,
            );
        }
        if let Some(path) = parsed.script {
            return std::process::ExitCode::from(
                (script::run_script_file(&path, parsed.args) & 0xff) as u8,
            );
        }
        // 无脚本：tty（或 -i）进交互模式，否则把 stdin 当脚本执行
        let stdin_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
        if parsed.interactive || stdin_tty {
            match Shell::run_shell() {
                Ok(code) => std::process::ExitCode::from(code),
                Err(_) => std::process::ExitCode::from(1),
            }
        } else {
            std::process::ExitCode::from((script::run_stdin() & 0xff) as u8)
        }
    }
}

impl Shell {
    fn run_shell() -> io::Result<u8> {
        // 注册 SIGINT handler（管道模式后备）
        executor::install_sigint_handler();

        // 注册 SIGCHLD handler：置位标志，由主循环回收并记录状态
        executor::install_sigchld_handler();

        // 注册信号 trap 处理器（INT/TERM/HUP 记录待处理信号）
        trap::install_handlers();

        // 作业控制：忽略 SIGTTIN/SIGTTOU（后台读终端不停止 shell），
        // 并确保 shell 处于自己的进程组、终端前台组指向 shell
        unsafe {
            libc::signal(libc::SIGTTIN, libc::SIG_IGN);
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            if libc::isatty(libc::STDIN_FILENO) == 1 {
                libc::setpgid(0, 0);
                libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp());
            }
        }

        // 初始化 PWD（若未设置）
        if std::env::var("PWD").is_err()
            && let Ok(cwd) = std::env::current_dir()
        {
            // SAFETY: shell 单线程
            unsafe {
                std::env::set_var("PWD", cwd.to_string_lossy().as_ref());
            }
        }

        // 加载 profile（路径可配置；默认 /etc/profile）
        let profile_path = &crate::config::load().paths.profile;
        let mut boot_rc: i32 = 0;
        let mut boot_history: Vec<String> = Vec::new();
        if std::path::Path::new(profile_path).exists() {
            source_file(profile_path, &mut boot_rc, &mut boot_history);
        }
        // 交互式启动文件：$HOME/.profile（若存在且与 /etc/profile 不同）
        if let Ok(home) = std::env::var("HOME") {
            let user_profile = format!("{}/.profile", home);
            if user_profile != *profile_path && std::path::Path::new(&user_profile).exists() {
                source_file(&user_profile, &mut boot_rc, &mut boot_history);
            }
        }

        let mut input = RawStdin;
        let mut last_rc: i32 = 0;
        let mut pending_line = String::new();

        // 命令历史：从文件加载
        let mut history: Vec<String> = load_history();
        let mut hist_idx: Option<usize> = None;
        let mut saved_line = String::new();

        // 复合命令（if/for/while）累积状态：block_depth > 0 表示块未闭合
        let mut block_lines: Vec<String> = Vec::new();
        let mut block_depth: i32 = 0;

        // raw mode guard（终端时启用，管道时为 None）
        let _raw_guard = enable_raw_mode();

        let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
        let _ = io::stdout().flush();

        let mut line = String::new();
        let mut cursor: usize = 0;

        loop {
            // 后台作业回收（SIGCHLD 置位）与信号 trap
            if jobs::sigchld_pending() {
                jobs::reap_children();
            }
            if let Some(sig) = trap::take_pending()
                && let Some(cmdline) = trap::get(sig)
            {
                let mut rc = last_rc;
                executor::execute_line(&cmdline, &mut rc, &history, &|_| {});
                last_rc = rc;
            }
            let mut byte = [0u8; 1];
            // 前台命令等待期间被 Ctrl-C 监控线程缓存的标准输入，优先消费
            // （保序队列，避免并发读 stdin 丢失/错位）
            let n = match executor::pending_stdin().lock().unwrap().pop_front() {
                Some(b) => {
                    byte[0] = b;
                    Ok(1)
                }
                None => input.read(&mut byte),
            };
            let n = match n {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                    if let Some(sig) = trap::take_pending()
                        && let Some(cmdline) = trap::get(sig)
                    {
                        let mut rc = last_rc;
                        executor::execute_line(&cmdline, &mut rc, &history, &|_| {});
                        last_rc = rc;
                        continue;
                    }
                    abort_line(&mut line, &mut cursor, &mut pending_line, &mut hist_idx);
                    continue;
                }
                Err(e) => return Err(e),
            };
            if n == 0 {
                break; // EOF
            }
            let b = byte[0];

            match b {
                b'\n' | b'\r' => {
                    let _ = writeln!(io::stdout());
                    let mut full_line = line.clone();
                    line.clear();
                    cursor = 0;

                    // 续行检查（感知引号：单引号内反斜杠不续行）
                    if needs_continuation(&full_line) {
                        pending_line = full_line.trim_end_matches('\\').to_string();
                        let _ = write!(io::stdout(), "{}", make_continuation_prompt());
                        let _ = io::stdout().flush();
                        continue;
                    } else if !pending_line.is_empty() {
                        full_line = format!("{}{}", pending_line, full_line);
                        pending_line.clear();
                    }

                    hist_idx = None;

                    // 空行直接跳过
                    if full_line.trim().is_empty() {
                        let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                        let _ = io::stdout().flush();
                        continue;
                    }

                    // 复合命令（if/for/while）：累积到完整块后整体执行
                    if block_depth > 0 || compound::is_compound_start(&full_line) {
                        block_depth += compound::nesting_delta(&full_line);
                        block_lines.push(full_line.clone());
                        if block_depth > 0 {
                            let _ = write!(io::stdout(), "{}", make_continuation_prompt());
                            let _ = io::stdout().flush();
                            continue;
                        }
                        let block = block_lines.join("\n");
                        block_lines.clear();
                        block_depth = 0;
                        last_rc =
                            compound::execute_block(&block, &mut last_rc, &history, &|rc: i32| {
                                let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                                let _ = io::stdout().flush();
                                run_exit_trap(&history, rc);
                                std::process::exit(rc);
                            });
                        if history.last() != Some(&block) {
                            history.push(block.clone());
                            append_history(&block);
                        }
                        let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                        let _ = io::stdout().flush();
                        continue;
                    }

                    // here-doc 处理：检测 <<DELIM
                    if full_line.contains("<<") {
                        full_line = process_heredoc(&full_line, &mut input);
                    }

                    // 执行行（历史扩展在 execute_line 内部完成）
                    last_rc =
                        executor::execute_line(&full_line, &mut last_rc, &history, &|rc: i32| {
                            let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                            let _ = io::stdout().flush();
                            run_exit_trap(&history, rc);
                            std::process::exit(rc);
                        });

                    // history -c：清空内存与历史文件
                    if take_history_clear() {
                        history.clear();
                        let _ = std::fs::write(history_file(), "");
                    }

                    // 存入历史（非空且与最后一条不同）
                    if !full_line.trim().is_empty() && history.last() != Some(&full_line) {
                        history.push(full_line.clone());
                        append_history(&full_line);
                    }

                    let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                    let _ = io::stdout().flush();
                }

                b'\t' => {
                    // Tab 补全
                    let (new_line, printed) = completion::tab_complete(&line);
                    if printed {
                        let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                        let _ = io::stdout().flush();
                    }
                    line = new_line;
                    cursor = line.len();
                    redraw(&pending_line, &line, cursor);
                }

                0x7f | 0x08 => {
                    // Backspace / Delete：删除光标前一个字符
                    if cursor > 0 {
                        // 移到上一个 UTF-8 字符边界
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

                0x03 => {
                    // Ctrl-C：中断当前行，新起一行
                    abort_line(&mut line, &mut cursor, &mut pending_line, &mut hist_idx);
                }

                0x1a => {
                    // Ctrl-Z：无前台进程时忽略（有前台进程时由监控线程处理挂起）
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
                    // Ctrl-U：删除光标前所有内容
                    if cursor > 0 {
                        line.drain(..cursor);
                        cursor = 0;
                        redraw(&pending_line, &line, cursor);
                    }
                }

                0x0b => {
                    // Ctrl-K：删除光标后所有内容
                    if cursor < line.len() {
                        line.truncate(cursor);
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

                0x1b => {
                    // ESC 序列：读取后续字节（方向键、Home/End 等）
                    let mut seq = [0u8; 2];
                    if input.read(&mut seq[..1]).unwrap_or(0) == 1
                        && seq[0] == b'['
                        && input.read(&mut seq[1..2]).unwrap_or(0) == 1
                    {
                        match seq[1] {
                            b'A' => {
                                // 上：上一条历史
                                if !history.is_empty() {
                                    if hist_idx.is_none() {
                                        saved_line = line.clone();
                                        hist_idx = Some(history.len());
                                    }
                                    if let Some(idx) = hist_idx
                                        && idx > 0
                                    {
                                        hist_idx = Some(idx - 1);
                                        line = history[idx - 1].clone();
                                        cursor = line.len();
                                        redraw(&pending_line, &line, cursor);
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
                                        hist_idx = None;
                                        line = saved_line.clone();
                                    }
                                    cursor = line.len();
                                    redraw(&pending_line, &line, cursor);
                                }
                            }
                            b'C' => {
                                // 右：光标右移（UTF-8 字符边界）
                                if cursor < line.len() {
                                    let mut next = cursor + 1;
                                    while next < line.len() && !line.is_char_boundary(next) {
                                        next += 1;
                                    }
                                    cursor = next;
                                    let _ = write!(io::stdout(), "\x1b[C");
                                    let _ = io::stdout().flush();
                                }
                            }
                            b'D' => {
                                // 左：光标左移（UTF-8 字符边界）
                                if cursor > 0 {
                                    let mut prev = cursor - 1;
                                    while prev > 0 && !line.is_char_boundary(prev) {
                                        prev -= 1;
                                    }
                                    cursor = prev;
                                    let _ = write!(io::stdout(), "\x1b[D");
                                    let _ = io::stdout().flush();
                                }
                            }
                            b'H' => {
                                cursor = 0;
                                redraw(&pending_line, &line, cursor);
                            }
                            b'F' => {
                                cursor = line.len();
                                redraw(&pending_line, &line, cursor);
                            }
                            b'1' | b'7' => {
                                let mut dummy = [0u8; 1];
                                let _ = input.read(&mut dummy);
                                cursor = 0;
                                redraw(&pending_line, &line, cursor);
                            }
                            b'4' | b'8' => {
                                let mut dummy = [0u8; 1];
                                let _ = input.read(&mut dummy);
                                cursor = line.len();
                                redraw(&pending_line, &line, cursor);
                            }
                            b'3' => {
                                // Delete 键
                                let mut dummy = [0u8; 1];
                                let _ = input.read(&mut dummy);
                                if cursor < line.len() {
                                    let mut end = cursor + 1;
                                    while end < line.len() && !line.is_char_boundary(end) {
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

                _ => {
                    // 普通可打印字符：聚合 UTF-8 多字节序列后插入到光标处
                    let ch = read_utf8_char(b, &mut input);
                    line.insert_str(cursor, &ch);
                    cursor += ch.len();
                    if cursor == line.len() {
                        let _ = write!(io::stdout(), "{}", ch);
                        let _ = io::stdout().flush();
                    } else {
                        redraw(&pending_line, &line, cursor);
                    }
                }
            }
        }

        run_exit_trap(&history, last_rc);
        Ok(last_rc as u8)
    }
}

/// 运行 EXIT trap（`trap 'cmd' EXIT`）；无 trap 时无操作。
fn run_exit_trap(history: &[String], last_rc: i32) {
    script::run_exit_trap(history, last_rc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_ascii_single_byte() {
        let mut input = &b"abc"[..];
        assert_eq!(read_utf8_char(b'a', &mut input), "a");
    }

    #[test]
    fn utf8_multibyte_char() {
        // '你' = E4 BD A0（三字节）
        let mut input = &b"\xbd\xa0"[..];
        assert_eq!(read_utf8_char(0xe4, &mut input), "你");
    }

    #[test]
    fn utf8_incomplete_sequence_uses_replacement() {
        // 首字节声明 3 字节但无后续 → U+FFFD
        let mut input = &b""[..];
        assert_eq!(read_utf8_char(0xe4, &mut input), "\u{fffd}");
    }

    #[test]
    fn utf8_two_byte_char() {
        // 'é' = C3 A9（两字节）
        let mut input = &b"\xa9"[..];
        assert_eq!(read_utf8_char(0xc3, &mut input), "é");
    }

    #[test]
    fn heredoc_operator_skips_here_string() {
        assert_eq!(find_heredoc_operator("cat <<< hi"), None);
        assert_eq!(find_heredoc_operator("cat <<EOF"), Some(4));
        assert_eq!(find_heredoc_operator("cat <<-EOF"), Some(4));
    }

    // ─── here-doc 操作符检测（引号感知）─────────

    #[test]
    fn heredoc_operator_plain() {
        assert_eq!(find_heredoc_operator("cat <<EOF"), Some(4));
    }

    #[test]
    fn heredoc_operator_inside_quotes_ignored() {
        // 引号内的 << 不是 here-doc
        assert_eq!(find_heredoc_operator("echo \"a << b\""), None);
        assert_eq!(find_heredoc_operator("echo 'a << b'"), None);
    }

    #[test]
    fn heredoc_operator_after_quotes_found() {
        assert_eq!(find_heredoc_operator("echo 'x' <<EOF"), Some(9));
    }

    // ─── 续行判断（引号感知）─────────────────────

    #[test]
    fn continuation_trailing_backslash() {
        assert!(needs_continuation("echo a\\"));
    }

    #[test]
    fn continuation_double_backslash_not_continuation() {
        assert!(!needs_continuation("echo a\\\\"));
    }

    #[test]
    fn continuation_inside_single_quotes_not_continuation() {
        assert!(!needs_continuation("echo 'a\\'"));
    }

    #[test]
    fn continuation_plain_line() {
        assert!(!needs_continuation("echo hello"));
    }

    // ─── source 历史隔离 ────────────────────────

    #[test]
    fn source_file_does_not_add_to_history() {
        let dir = std::env::temp_dir().join(format!("rbox_source_hist_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.sh");
        std::fs::write(&f, "echo hi\n").unwrap();
        let mut rc = 0;
        let mut history = vec!["pre-existing".to_string()];
        let ret = source_file(f.to_str().unwrap(), &mut rc, &mut history);
        assert_eq!(ret, 0);
        // source 的行不应进入交互式历史
        assert_eq!(history, vec!["pre-existing".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expand_tilde_path_works() {
        let orig = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", "/root") };
        assert_eq!(expand_tilde_path("~"), "/root");
        assert_eq!(expand_tilde_path("~/.rbox_history"), "/root/.rbox_history");
        assert_eq!(expand_tilde_path("/var/hist"), "/var/hist");
        match orig {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn expand_tilde_path_without_home_keeps_original() {
        let orig = std::env::var_os("HOME");
        unsafe { std::env::remove_var("HOME") };
        assert_eq!(expand_tilde_path("~/.rbox_history"), "/.rbox_history");
        assert_eq!(expand_tilde_path("~"), "~");
        match orig {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
