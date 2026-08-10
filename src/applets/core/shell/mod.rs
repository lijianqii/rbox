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

mod builtin;
mod completion;
mod executor;
mod expander;
mod parser;
mod reader;
mod tokenizer;
mod types;

use crate::applet::Applet;
use reader::{enable_raw_mode, make_prompt, redraw};
use std::io::{self, BufRead, Read, Write};

/// 处理 here-doc：检测 `<<DELIM`，读取后续行直到 DELIM，写入临时文件。
/// 返回替换后的命令行（`<<DELIM` -> `<tmpfile`）。
fn process_heredoc<R: BufRead>(line: &str, input: &mut R) -> String {
    // 查找 << 在行中的位置
    let idx = match line.find("<<") {
        Some(i) => i,
        None => return line.to_string(),
    };

    // 提取 delimiter（<< 后面的第一个词）
    let after = &line[idx + 2..];
    let delim = after.split_whitespace().next().unwrap_or("");
    if delim.is_empty() {
        return line.to_string();
    }

    // 读取 here-doc 内容（按行读取，保留 UTF-8）
    let mut content = String::new();
    loop {
        let _ = write!(io::stdout(), "> ");
        let _ = io::stdout().flush();
        let mut line_buf = Vec::new();
        if input.read_until(b'\n', &mut line_buf).unwrap_or(0) == 0 {
            break; // EOF
        }
        // 去掉行尾换行符（\n 或 \r\n）
        if line_buf.last() == Some(&b'\n') {
            line_buf.pop();
            if line_buf.last() == Some(&b'\r') {
                line_buf.pop();
            }
        }
        let line_content = String::from_utf8_lossy(&line_buf).trim().to_string();
        if line_content == delim {
            break;
        }
        content.push_str(&line_content);
        content.push('\n');
    }

    // 写入临时文件
    let tmpfile = format!("/tmp/heredoc_{}", std::process::id());
    if std::fs::write(&tmpfile, &content).is_err() {
        return line.to_string();
    }

    // 替换 <<DELIM 为 <tmpfile
    let before = &line[..idx];
    format!("{} < {}", before.trim_end(), tmpfile)
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

/// 历史文件路径。
fn history_file() -> String {
    std::env::var("HOME")
        .map(|h| format!("{}/.rbox_history", h))
        .unwrap_or_else(|_| "/tmp/.rbox_history".to_string())
}

/// 加载历史文件。
fn load_history() -> Vec<String> {
    let path = history_file();
    match std::fs::read_to_string(&path) {
        Ok(content) => content.lines().map(|l| l.to_string()).collect(),
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
fn source_file(path: &str, last_rc: &mut i32, history: &mut Vec<String>) -> i32 {
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
        *last_rc = executor::execute_line(line, last_rc, history, |_rc: i32| {
            // source 中不支持 exit
        });
        // 追加历史
        if !line.is_empty() && history.last() != Some(&line.to_string()) {
            history.push(line.to_string());
            append_history(line);
        }
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

    fn run(&self, _args: &[String]) -> std::process::ExitCode {
        match Shell::run_shell() {
            Ok(code) => std::process::ExitCode::from(code),
            Err(_) => std::process::ExitCode::from(1),
        }
    }
}

impl Shell {
    fn run_shell() -> io::Result<u8> {
        // 注册 SIGINT handler（管道模式后备）
        executor::install_sigint_handler();

        // 注册 SIGCHLD handler：自动回收后台僵尸子进程
        executor::install_sigchld_handler();

        // 加载 /etc/profile（如果存在）
        let mut boot_rc: i32 = 0;
        let mut boot_history: Vec<String> = Vec::new();
        if std::path::Path::new("/etc/profile").exists() {
            source_file("/etc/profile", &mut boot_rc, &mut boot_history);
        }

        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut last_rc: i32 = 0;
        let mut pending_line = String::new();

        // 命令历史：从文件加载
        let mut history: Vec<String> = load_history();
        let mut hist_idx: Option<usize> = None;
        let mut saved_line = String::new();

        // raw mode guard（终端时启用，管道时为 None）
        let _raw_guard = enable_raw_mode();

        let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
        let _ = io::stdout().flush();

        let mut line = String::new();
        let mut cursor: usize = 0;

        loop {
            let mut byte = [0u8; 1];
            let n = match input.read(&mut byte) {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
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

                    // 续行检查
                    if full_line.ends_with('\\') && !full_line.ends_with("\\\\") {
                        pending_line = full_line.trim_end_matches('\\').to_string();
                        let _ = write!(io::stdout(), "> ");
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

                    // source 命令特殊处理
                    let trimmed = full_line.trim();
                    if trimmed.starts_with("source ") || trimmed.starts_with(". ") {
                        let file = trimmed.split_whitespace().nth(1).unwrap_or("");
                        source_file(file, &mut last_rc, &mut history);
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
                        executor::execute_line(&full_line, &mut last_rc, &history, |rc: i32| {
                            let _ = write!(io::stdout(), "{}", make_prompt(&pending_line));
                            let _ = io::stdout().flush();
                            std::process::exit(rc);
                        });

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
                    // 普通可打印字符：插入到光标处
                    line.insert(cursor, b as char);
                    cursor += 1;
                    if cursor == line.len() {
                        let _ = write!(io::stdout(), "{}", b as char);
                        let _ = io::stdout().flush();
                    } else {
                        redraw(&pending_line, &line, cursor);
                    }
                }
            }
        }

        Ok(last_rc as u8)
    }
}
