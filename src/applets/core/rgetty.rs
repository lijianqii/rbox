//! `rgetty` - 终端登录提示程序。
//!
//! 在指定终端（默认当前 stdin）上循环打印 `rbox login: ` 提示，
//! 读取用户名后 exec `rlogin`。登录失败时 rlogin 退出，init 的
//! `Console = true` 服务会自动重新拉起 rgetty，形成登录循环；
//! 登录成功时 rlogin 会继续 exec 用户 shell，shell 退出后同样由
//! init respawn，回到登录提示。

use crate::applet::Applet;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::ExitCode;

pub struct Getty;
pub static GETTY: &Getty = &Getty;

impl Applet for Getty {
    fn name(&self) -> &'static str {
        "rgetty"
    }
    fn help(&self) -> &'static str {
        "rgetty [TTY] - print login prompt and exec rlogin"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        if let Some(tty) = args.first()
            && let Err(e) = setup_tty(tty)
        {
            eprintln!("rgetty: cannot open {}: {}", tty, e);
            return ExitCode::FAILURE;
        }
        run_getty()
    }
}

/// 打开指定 tty 并复制到 stdin/stdout/stderr。
fn setup_tty(path: &str) -> io::Result<()> {
    use std::ffi::CString;
    let c = CString::new(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        if unsafe { libc::dup2(fd, target) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    unsafe { libc::close(fd) };
    Ok(())
}

/// 把终端恢复为合理的行缓冲模式（关闭可能残留的 raw/cbreak 设置）。
fn reset_terminal() {
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } != 0 {
        return;
    }
    term.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG;
    term.c_lflag &= !libc::ECHONL;
    term.c_cc[libc::VMIN] = 1;
    term.c_cc[libc::VTIME] = 0;
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) };
}

/// 主循环：打印提示、读取用户名、exec rlogin。
fn run_getty() -> ExitCode {
    loop {
        reset_terminal();
        let _ = write!(io::stdout(), "\r\nrbox login: ");
        let _ = io::stdout().flush();

        let mut user = String::new();
        let n = io::stdin().lock().read_line(&mut user).unwrap_or(0);
        if n == 0 {
            // EOF：退出，由 init respawn
            return ExitCode::SUCCESS;
        }
        let Some(user) = normalize_username(&user) else {
            continue;
        };

        // exec rlogin（替换当前进程）；失败时继续循环重新提示
        let err = std::process::Command::new("/bin/rlogin").arg(&user).exec();
        eprintln!("rgetty: cannot exec rlogin: {}", err);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// 规范化用户名：去掉首尾空白，空输入返回 None。
fn normalize_username(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(GETTY.name(), "rgetty");
        assert!(GETTY.help().contains("rlogin"));
    }

    #[test]
    fn normalize_username_basic() {
        assert_eq!(normalize_username("root\n"), Some("root".to_string()));
        assert_eq!(
            normalize_username("  alice \r\n"),
            Some("alice".to_string())
        );
    }

    #[test]
    fn normalize_username_empty() {
        assert_eq!(normalize_username(""), None);
        assert_eq!(normalize_username("   \n"), None);
        assert_eq!(normalize_username("\r\n"), None);
    }
}
