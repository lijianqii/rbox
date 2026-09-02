//! `rgetty` - 终端登录提示程序。
//!
//! 在终端上循环打印 `rbox login: ` 提示，读取用户名后 exec `rlogin`。
//! 登录失败时 rlogin 退出，init 的 `Console = true` 服务会自动重新拉起
//! rgetty，形成登录循环；登录成功时 rlogin 会继续 exec 用户 shell，
//! shell 退出后同样由 init respawn，回到登录提示。
//!
//! 终端选择优先级：
//! 1. 显式参数 `rgetty [TTY]`；
//! 2. 内核信息：`/sys/class/tty/console/active`（实际激活的 console），
//!    回退解析 `/proc/cmdline` 的 `console=` 参数；
//! 3. 以上均不可用时继承父进程的 stdin/stdout/stderr（init console 服务）。

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
        "rgetty [TTY] - login prompt (tty auto-detected from kernel console)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let explicit: Option<String> = args.first().cloned();
        // 优先级：显式参数 > 内核实际激活的 console > console= 内核参数 > 继承 stdio
        let tty = explicit
            .clone()
            .or_else(active_console_tty)
            .or_else(console_tty_from_cmdline);
        if let Some(tty) = tty {
            match setup_tty(&tty) {
                Ok(()) => {}
                Err(e) => {
                    if explicit.is_some() {
                        // 显式指定的 tty 打不开：直接失败（由 init respawn）
                        eprintln!("rgetty: cannot open {}: {}", tty, e);
                        return ExitCode::FAILURE;
                    }
                    // 内核信息推导的 tty 打不开：回退继承的 stdio
                    eprintln!("rgetty: cannot open {}: {}, using inherited stdio", tty, e);
                }
            }
        }
        run_getty()
    }
}

/// 从 /sys/class/tty/console/active 读取内核实际激活的 console 设备名。
pub(crate) fn active_console_names(content: &str) -> Vec<String> {
    content.split_whitespace().map(|s| s.to_string()).collect()
}

/// 从激活的 console 列表中选一个可交互的 tty 路径。
/// 虚拟终端 tty0/tty 在无显示环境没有意义，跳过。
fn pick_console_path(names: &[String]) -> Option<String> {
    names
        .iter()
        .find(|n| n.as_str() != "tty0" && n.as_str() != "tty")
        .map(|n| format!("/dev/{}", n))
}

/// 读取内核实际激活的 console 并映射为 /dev 路径。
fn active_console_tty() -> Option<String> {
    let content = std::fs::read_to_string("/sys/class/tty/console/active").ok()?;
    let names = active_console_names(&content);
    let path = pick_console_path(&names)?;
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}

/// 解析内核命令行中的 `console=` 参数，返回设备名列表（去掉波特率等后缀）。
pub(crate) fn console_devices(cmdline: &str) -> Vec<String> {
    cmdline
        .split_whitespace()
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            if k != "console" {
                return None;
            }
            // 处理盲文终端前缀 `brl,` 与波特率后缀 `,115200n8`
            let v = v.strip_prefix("brl,").unwrap_or(v);
            let name = v.split(',').next().unwrap_or("");
            if name.is_empty() || name == "null" {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// 把 console 设备名映射为 /dev 路径；取第一个（内核的主 console）。
pub(crate) fn console_tty_path(devices: &[String]) -> Option<String> {
    let name = devices.first()?;
    if name == "tty0" || name == "tty" {
        Some("/dev/console".to_string())
    } else {
        Some(format!("/dev/{}", name))
    }
}

/// 从 /proc/cmdline 的 `console=` 推导 tty 路径。
fn console_tty_from_cmdline() -> Option<String> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").ok()?;
    let path = console_tty_path(&console_devices(&cmdline))?;
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        None
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
        assert!(GETTY.help().contains("login prompt"));
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

    // ─── 内核 console 推导 ─────────────────────

    #[test]
    fn console_devices_parses() {
        assert_eq!(
            console_devices("console=ttyAMA0 rdinit=/init"),
            vec!["ttyAMA0"]
        );
        assert_eq!(
            console_devices("console=ttyS0,115200n8 console=ttyAMA0"),
            vec!["ttyS0", "ttyAMA0"]
        );
        // console=null 与无 console= 参数
        assert_eq!(console_devices("console=null quiet"), Vec::<String>::new());
        assert_eq!(console_devices("root=/dev/ram0"), Vec::<String>::new());
    }

    #[test]
    fn console_devices_handles_brl_prefix() {
        assert_eq!(console_devices("console=brl,ttyS0"), vec!["ttyS0"]);
    }

    #[test]
    fn console_tty_path_maps_first() {
        let devs = console_devices("console=ttyAMA0");
        assert_eq!(console_tty_path(&devs), Some("/dev/ttyAMA0".to_string()));
        // VT 映射为 /dev/console
        let v = vec!["tty0".to_string()];
        assert_eq!(console_tty_path(&v), Some("/dev/console".to_string()));
        assert_eq!(console_tty_path(&[]), None);
    }

    #[test]
    fn active_console_names_parses() {
        let names = active_console_names("ttyAMA0\n");
        assert_eq!(names, vec!["ttyAMA0"]);
        let multi = active_console_names("ttyAMA0 ttyS0\n");
        assert_eq!(multi, vec!["ttyAMA0", "ttyS0"]);
    }

    #[test]
    fn pick_console_path_skips_vt() {
        let names = active_console_names("tty0\n");
        assert_eq!(pick_console_path(&names), None);
        let names = active_console_names("tty0 ttyAMA0\n");
        assert_eq!(pick_console_path(&names), Some("/dev/ttyAMA0".to_string()));
    }
}
