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
    // 关闭 ICANON + ECHO，但保留 ISIG（让 Ctrl-C 产生 SIGINT 信号）
    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
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
pub fn redraw(pending: &str, line: &str, cursor: usize) {
    let prompt = if pending.is_empty() { "rbox# " } else { "> " };
    let _ = write!(io::stdout(), "\r\x1b[K{}{}", prompt, line);
    // 光标定位：从行末向左移动到 cursor 位置
    let display_pos = display_width(&line[..cursor]);
    let back = display_width(line) - display_pos;
    if back > 0 {
        let _ = write!(io::stdout(), "\x1b[{}D", back);
    }
    let _ = io::stdout().flush();
}
