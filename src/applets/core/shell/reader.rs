//! 终端 raw 模式管理 + 光标重绘。

use libc::{TCSANOW, tcgetattr, tcsetattr, termios};
use std::io::{self, Write};
use std::os::fd::AsRawFd;

/// RAII guard：drop 时恢复原始终端设置。
pub struct RawGuard {
    fd: i32,
    original: termios,
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: restoring saved termios state
        unsafe {
            tcsetattr(self.fd, TCSANOW, &self.original);
        }
    }
}

/// 将 stdin 设为 cbreak 模式（关闭 ICANON + ECHO，VMIN=1 VTIME=0）。
/// 如果 stdin 不是终端（管道/文件），返回 None，不影响读取。
pub fn enable_raw_mode() -> Option<RawGuard> {
    let fd = io::stdin().as_raw_fd();
    let mut original: termios = unsafe { std::mem::zeroed() };
    if unsafe { tcgetattr(fd, &mut original) } != 0 {
        return None;
    }
    let mut raw = original;
    // 关闭 ICANON + ECHO + ISIG
    // ISIG 关闭后 Ctrl-C 不再产生 SIGINT 信号，而是作为 0x03 字节传递给 shell
    // shell 在 REPL 中检测 0x03 后手动向子进程组发送 SIGINT
    raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    unsafe {
        tcsetattr(fd, TCSANOW, &raw);
    }
    Some(RawGuard { fd, original })
}

/// 计算字符串在终端中的显示宽度（ASCII 为 1，其他字符按 1 计算）。
pub fn display_width(s: &str) -> usize {
    s.chars().count()
}

/// 重绘当前行：`\r` + 清除行 + prompt + line + 光标定位。
/// `pending` 非空表示续行模式，显示 `> ` 提示符；否则使用 `rbox# ` 或 $PS1。
pub fn redraw(pending: &str, line: &str, cursor: usize) {
    let prompt = make_prompt(pending);
    let _ = write!(io::stdout(), "\r\x1b[K{}{}", prompt, line);
    // 光标定位：从行末向左移动到 cursor 位置
    let display_pos = display_width(&line[..cursor]);
    let back = display_width(line) - display_pos;
    if back > 0 {
        let _ = write!(io::stdout(), "\x1b[{}D", back);
    }
    let _ = io::stdout().flush();
}

/// 生成提示符字符串。
pub fn make_prompt(pending: &str) -> String {
    if !pending.is_empty() {
        return "> ".to_string();
    }
    if let Ok(ps1) = std::env::var("PS1") {
        expand_ps1(&ps1)
    } else {
        "rbox# ".to_string()
    }
}

/// 展开 PS1 转义序列。
pub fn expand_ps1(ps1: &str) -> String {
    let mut result = String::new();
    let mut chars = ps1.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                chars.next();
                match next {
                    'w' => {
                        let cwd = std::env::current_dir()
                            .map(|d| d.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let home = std::env::var("HOME").unwrap_or_default();
                        if !home.is_empty() && cwd.starts_with(&home) {
                            result.push('~');
                            result.push_str(&cwd[home.len()..]);
                        } else {
                            result.push_str(&cwd);
                        }
                    }
                    'u' => {
                        result.push_str(
                            &std::env::var("USER").unwrap_or_else(|_| "root".to_string()),
                        );
                    }
                    'h' => {
                        let host = std::fs::read_to_string("/etc/hostname")
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        result.push_str(&host);
                    }
                    '$' => {
                        result.push('$');
                    }
                    '#' => {
                        result.push('#');
                    }
                    'n' => {
                        result.push('\n');
                    }
                    'e' => {
                        result.push('\u{1b}');
                    }
                    '\\' => {
                        result.push('\\');
                    }
                    _ => {
                        result.push('\\');
                        result.push(next);
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps1_literal() {
        assert_eq!(expand_ps1("hello"), "hello");
    }

    #[test]
    fn ps1_user() {
        unsafe {
            std::env::set_var("USER", "testuser");
        }
        assert_eq!(expand_ps1(r"\u"), "testuser");
        unsafe {
            std::env::remove_var("USER");
        }
    }

    #[test]
    fn ps1_user_default_root() {
        unsafe {
            std::env::remove_var("USER");
        }
        assert_eq!(expand_ps1(r"\u"), "root");
    }

    #[test]
    fn ps1_dollar() {
        assert_eq!(expand_ps1(r"\$"), "$");
    }

    #[test]
    fn ps1_hash() {
        assert_eq!(expand_ps1(r"\#"), "#");
    }

    #[test]
    fn ps1_newline() {
        assert_eq!(expand_ps1(r"\n"), "\n");
    }

    #[test]
    fn ps1_escape() {
        assert_eq!(expand_ps1(r"\e"), "\u{1b}");
    }

    #[test]
    fn ps1_backslash() {
        assert_eq!(expand_ps1(r"\\"), "\\");
    }

    #[test]
    fn ps1_unknown_escape_preserved() {
        assert_eq!(expand_ps1(r"\x"), "\\x");
    }

    #[test]
    fn ps1_trailing_backslash() {
        // trailing \ with no following char is dropped (consumed by peek but no push)
        assert_eq!(expand_ps1("abc\\"), "abc");
    }

    #[test]
    fn make_prompt_continuation() {
        assert_eq!(make_prompt("pending text"), "> ");
    }

    #[test]
    fn make_prompt_default() {
        unsafe {
            std::env::remove_var("PS1");
        }
        assert_eq!(make_prompt(""), "rbox# ");
    }

    #[test]
    fn make_prompt_ps1() {
        unsafe {
            std::env::set_var("PS1", r"\u@\h:\w$ ");
        }
        let prompt = make_prompt("");
        assert!(prompt.contains("@"));
        assert!(prompt.contains("$"));
        unsafe {
            std::env::remove_var("PS1");
        }
    }

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
    }
}
