//! `rgetty` - 终端登录提示程序。
//!
//! 在终端上循环打印 `rbox login: ` 提示，读取用户名后 exec `rlogin`。
//! 登录失败时 rlogin 退出，init 的 `Console = true` 服务会自动重新拉起
//! rgetty，形成登录循环；登录成功时 rlogin 会继续 exec 用户 shell，
//! shell 退出后同样由 init respawn，回到登录提示。
//!
//! 用法：`rgetty [-L] [-t SEC] [TTY]`
//! - `-L`：设置 CLOCAL（忽略载波检测，真实串口常用，同 busybox getty）；
//! - `-t SEC`：超过 SEC 秒未输入用户名则退出（由 init respawn），防僵尸占终端；
//! - `TTY`：可写裸设备名（`ttyAMA0`）或完整路径（`/dev/ttyAMA0`）。
//!
//! 终端选择优先级：
//! 1. service 配置（`[Service] TTY = ...`，经 init 拼成命令行参数传入）；
//! 2. 内核信息：`/sys/class/tty/console/active`（实际激活的 console），
//!    回退解析 `/proc/cmdline` 的 `console=` 参数；
//! 3. 以上均不可用时继承父进程的 stdin/stdout/stderr（init console 服务）。

use crate::applet::Applet;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::ExitCode;

pub struct Getty;
pub static GETTY: &Getty = &Getty;

/// rgetty 命令行选项。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct GettyOpts {
    /// `-t SEC`：读取用户名的超时秒数；None 表示不超时。
    pub(crate) timeout_secs: Option<u64>,
    /// `-L`：设置 CLOCAL（忽略载波检测）。
    pub(crate) clocal: bool,
    /// 位置参数：指定的 tty（service 配置传入或命令行显式给出）。
    pub(crate) tty: Option<String>,
}

impl Applet for Getty {
    fn name(&self) -> &'static str {
        "rgetty"
    }
    fn help(&self) -> &'static str {
        "rgetty [-L] [-t SEC] [TTY] - login prompt (tty from service config or kernel console)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let opts = match parse_args(args) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("rgetty: {}", e);
                eprintln!("usage: rgetty [-L] [-t SEC] [TTY]");
                return ExitCode::FAILURE;
            }
        };
        // 优先级：service/显式参数 > 内核实际激活的 console > console= 内核参数 > 继承 stdio
        let tty = opts
            .tty
            .clone()
            .or_else(active_console_tty)
            .or_else(console_tty_from_cmdline);
        if let Some(tty) = tty {
            let path = normalize_tty_path(&tty);
            match setup_tty(&path) {
                Ok(()) => {}
                Err(e) => {
                    if opts.tty.is_some() {
                        // service/显式指定的 tty 打不开：直接失败（由 init respawn）
                        eprintln!("rgetty: cannot open {}: {}", path, e);
                        return ExitCode::FAILURE;
                    }
                    // 内核信息推导的 tty 打不开：回退继承的 stdio
                    eprintln!("rgetty: cannot open {}: {}, using inherited stdio", path, e);
                }
            }
        }
        if opts.clocal {
            set_clocal();
        }
        run_getty(opts.timeout_secs)
    }
}

/// 解析 rgetty 命令行：`[-L] [-t SEC] [TTY]`。
pub(crate) fn parse_args(args: &[String]) -> Result<GettyOpts, String> {
    let mut opts = GettyOpts::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-L" => opts.clocal = true,
            "-t" => {
                i += 1;
                let v = args.get(i).ok_or("option -t requires SECONDS")?;
                opts.timeout_secs =
                    Some(v.parse().map_err(|_| format!("invalid -t value: {}", v))?);
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option: {}", s));
            }
            _ => {
                if opts.tty.is_none() {
                    opts.tty = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }
    Ok(opts)
}

/// 把 tty 参数规范化为 /dev 路径：`ttyAMA0` → `/dev/ttyAMA0`。
pub(crate) fn normalize_tty_path(tty: &str) -> String {
    if tty.starts_with('/') {
        tty.to_string()
    } else {
        format!("/dev/{}", tty)
    }
}

/// 设置 CLOCAL（忽略载波检测）。非 tty 时静默跳过。
fn set_clocal() {
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } != 0 {
        return;
    }
    term.c_cflag |= libc::CLOCAL;
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) };
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

/// 主循环：打印提示、读取用户名（带超时）、exec rlogin。
fn run_getty(timeout_secs: Option<u64>) -> ExitCode {
    loop {
        reset_terminal();
        let _ = write!(io::stdout(), "\r\nrbox login: ");
        let _ = io::stdout().flush();

        let Some(user) = read_username(timeout_secs) else {
            // 超时或 EOF：退出，由 init respawn
            if timeout_secs.is_some() {
                let _ = writeln!(io::stdout(), "timed out, respawning");
            }
            return ExitCode::SUCCESS;
        };
        let Some(user) = normalize_username(&user) else {
            continue;
        };

        // exec rlogin（替换当前进程）；失败时继续循环重新提示
        let err = std::process::Command::new("/bin/rlogin").arg(&user).exec();
        eprintln!("rgetty: cannot exec rlogin: {}", err);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// 读取一行用户名；超过 timeout_secs 未输入或 EOF 时返回 None。
/// 用 poll 实现超时，避免设置终端 VTIME 影响其他行为。
fn read_username(timeout_secs: Option<u64>) -> Option<String> {
    let fd = libc::STDIN_FILENO;
    let deadline =
        timeout_secs.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));
    let mut line: Vec<u8> = Vec::new();
    loop {
        if let Some(d) = deadline {
            let now = std::time::Instant::now();
            if now >= d {
                return None;
            }
            let mut fds = [libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            let ms = (d - now).as_millis().min(i32::MAX as u128) as i32;
            if unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) } <= 0 {
                return None; // 超时或 poll 错误
            }
        }
        let mut b = [0u8; 1];
        let n = unsafe { libc::read(fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n <= 0 {
            return None; // EOF / 读错误
        }
        if b[0] == b'\n' || b[0] == b'\r' {
            break;
        }
        line.push(b[0]);
    }
    Some(String::from_utf8_lossy(&line).into_owned())
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
    fn parse_args_basic() {
        let opts = parse_args(&[]).unwrap();
        assert_eq!(opts, GettyOpts::default());

        let opts = parse_args(&["ttyAMA0".to_string()]).unwrap();
        assert_eq!(opts.tty.as_deref(), Some("ttyAMA0"));
    }

    #[test]
    fn parse_args_options() {
        let opts = parse_args(&["-L".to_string(), "-t".to_string(), "60".to_string()]).unwrap();
        assert!(opts.clocal);
        assert_eq!(opts.timeout_secs, Some(60));

        let opts = parse_args(&[
            "-t".to_string(),
            "30".to_string(),
            "-L".to_string(),
            "ttyS0".to_string(),
        ])
        .unwrap();
        assert!(opts.clocal);
        assert_eq!(opts.timeout_secs, Some(30));
        assert_eq!(opts.tty.as_deref(), Some("ttyS0"));
    }

    #[test]
    fn parse_args_errors() {
        assert!(parse_args(&["-t".to_string()]).is_err());
        assert!(parse_args(&["-t".to_string(), "abc".to_string()]).is_err());
        assert!(parse_args(&["-x".to_string()]).is_err());
    }

    #[test]
    fn normalize_tty_path_works() {
        assert_eq!(normalize_tty_path("ttyAMA0"), "/dev/ttyAMA0");
        assert_eq!(normalize_tty_path("/dev/ttyS0"), "/dev/ttyS0");
        assert_eq!(normalize_tty_path("console"), "/dev/console");
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
