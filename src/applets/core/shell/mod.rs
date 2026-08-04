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
use reader::{enable_raw_mode, redraw};
use std::io::{self, Read, Write};

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
        // 注册 SIGINT 处理器：前台有子进程时转发信号。
        // tty 模式下 Ctrl-C 产生 SIGINT（ISIG 开启），由 handler 处理；
        // 管道模式下 0x03 作为普通字节到达 REPL 的 0x03 分支。
        executor::install_sigint_handler();

        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut last_rc: i32 = 0;
        let mut pending_line = String::new();

        // 命令历史
        let mut history: Vec<String> = Vec::new();
        let mut hist_idx: Option<usize> = None;
        let mut saved_line = String::new();

        // raw mode guard（终端时启用，管道时为 None）
        let _raw_guard = enable_raw_mode();

        let _ = write!(io::stdout(), "rbox# ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        let mut cursor: usize = 0;

        loop {
            let mut byte = [0u8; 1];
            let n = match input.read(&mut byte) {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                    // SIGINT 打断了 read：如果有前台进程已被 handler 处理；
                    // 如果是在编辑行时，清除当前行并新起一行
                    let _ = writeln!(io::stdout(), "^C");
                    line.clear();
                    cursor = 0;
                    pending_line.clear();
                    hist_idx = None;
                    let _ = write!(io::stdout(), "rbox# ");
                    let _ = io::stdout().flush();
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
                        let _ = write!(io::stdout(), "rbox# ");
                        let _ = io::stdout().flush();
                        continue;
                    }

                    // 执行行（历史扩展在 execute_line 内部完成）
                    last_rc =
                        executor::execute_line(&full_line, &mut last_rc, &history, |rc: i32| {
                            let _ = write!(io::stdout(), "rbox# ");
                            let _ = io::stdout().flush();
                            std::process::exit(rc);
                        });

                    // 存入历史（非空且与最后一条不同）
                    if !full_line.trim().is_empty() && history.last() != Some(&full_line) {
                        history.push(full_line.clone());
                    }

                    let _ = write!(io::stdout(), "rbox# ");
                    let _ = io::stdout().flush();
                }

                b'\t' => {
                    // Tab 补全
                    let (new_line, printed) = completion::tab_complete(&line);
                    if printed {
                        // 多匹配列表已打印，重绘提示符 + 当前行
                        let _ = write!(io::stdout(), "rbox# ");
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
