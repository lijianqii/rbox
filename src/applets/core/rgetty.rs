//! `rgetty` - 终端登录提示程序。
//!
//! 在终端上循环打印登录提示（如 `rbox login: `），读取用户名后 fork 子进程
//! 执行 `rlogin`；rgetty 常驻，登录失败/会话超时/shell 退出后原地重新提示。
//! init 的 `Restart = "always"` 仅兜底 rgetty 本身崩溃/被杀。
//!
//! 用法：`rgetty [-L] [-t SEC] [TTY]`
//! - `-L`：设置 CLOCAL（忽略载波检测，真实串口常用，同 busybox getty）；
//! - `-t SEC`：**登录会话空闲超时**（无输入达到 SEC 秒自动登出回到登录提示，
//!   登录提示阶段不超时；有输入活动会刷新计时，只对"空闲"会话生效）；
//! - `TTY`：可写裸设备名（`ttyAMA0`）或完整路径（`/dev/ttyAMA0`）。
//!
//! 终端选择：仅使用命令行显式 TTY 参数（由 `ExecStart` 完整命令传入）；
//! 未指定 TTY 时使用继承的 stdin/stdout/stderr（init console 服务的 stdio 即登录终端）。

use crate::applet::Applet;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::ExitCode;

pub struct Getty;
pub static GETTY: &Getty = &Getty;

/// rgetty 命令行选项。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct GettyOpts {
    /// `-t SEC`：登录会话空闲超时秒数（无输入达到该时长自动登出）；None 表示不超时。
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
        "rgetty [-L] [-t SEC] [TTY] - login prompt on TTY (session timeout via -t)"
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
        let cfg = crate::config::load().getty.clone();
        // 只使用命令行显式指定的 TTY；未指定时使用继承的 stdin/stdout/stderr
        // （init 服务继承的 stdio 即登录终端）。
        if let Some(tty) = &opts.tty {
            let path = normalize_tty_path(tty);
            if let Err(e) = setup_tty(&path) {
                eprintln!("rgetty: cannot open {}: {}", path, e);
                return ExitCode::FAILURE;
            }
        }
        if opts.clocal {
            set_clocal();
        }
        // 超时：命令行 -t 优先，缺省用配置 default_timeout
        let timeout = opts.timeout_secs.or(cfg.default_timeout);
        run_getty(&cfg, timeout)
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
                let secs: u64 = v.parse().map_err(|_| format!("invalid -t value: {}", v))?;
                // 0 表示不超时（避免 0 秒超时导致无限重试刷屏）
                opts.timeout_secs = (secs > 0).then_some(secs);
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

/// 打印登录前横幅（/etc/issue，配置可改；文件不存在则跳过）。
fn print_issue(issue_file: &str) {
    if let Ok(content) = std::fs::read_to_string(issue_file) {
        let _ = write!(io::stdout(), "{}", content);
        let _ = io::stdout().flush();
    }
}

/// 主循环：打印提示、读取用户名（登录提示阶段不超时）、fork 子进程执行 rlogin。
/// rgetty 常驻：登录失败/会话超时/shell 退出后原地重新提示，
/// 不经过 init 重启（init 的 Restart=always 仅兜底 rgetty 本身崩溃）。
/// `-t` 超时为空闲超时：会话期间有输入活动会刷新计时，持续无输入才登出。
fn run_getty(cfg: &crate::config::GettyConfig, timeout_secs: Option<u64>) -> ExitCode {
    print_issue(&cfg.issue_file);
    loop {
        reset_terminal();
        let _ = write!(io::stdout(), "\r\n{}", cfg.prompt);
        let _ = io::stdout().flush();

        let Some(user) = read_username() else {
            // EOF（如串口断开）：稍等后原地重新提示，避免忙循环
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        };
        let Some(user) = normalize_username(&user) else {
            continue;
        };

        // fork 子进程执行 rlogin（子进程内 exec shell），父进程等待；
        // 登录失败（非零退出）延迟后再提示，防暴力刷屏。
        run_login(&user, cfg, timeout_secs);
    }
}

/// fork 子进程执行登录程序；父进程等待（空闲超时从登录成功开始计时）。
fn run_login(user: &str, cfg: &crate::config::GettyConfig, timeout_secs: Option<u64>) {
    // 通知管道：rlogin 在 exec 用户 shell 前写 1 字节，
    // rgetty 据此确定"登录成功"并开始空闲计时（登录过程不计时）
    let mut notify = [-1i32; 2];
    if unsafe { libc::pipe(notify.as_mut_ptr()) } != 0 {
        notify = [-1, -1];
    }
    let pid = unsafe {
        let pid = libc::fork();
        if pid < 0 {
            eprintln!("rgetty: fork failed");
            std::thread::sleep(std::time::Duration::from_secs(1));
            return;
        }
        if pid == 0 {
            // 子进程：创建独立会话/进程组（自身 pid 即 pgid），
            // 使会话超时能终止整个登录会话（含 shell 派生的后台进程）
            libc::setsid();
            // 关闭读端，把写端 fd 传给 rlogin（exec 后保留）
            if notify[0] >= 0 {
                libc::close(notify[0]);
            }
            // exec 登录程序（成功则被替换，不会返回）
            let mut cmd = std::process::Command::new(&cfg.login_program);
            cmd.arg(user);
            if notify[1] >= 0 {
                cmd.env("RBOX_LOGIN_NOTIFY_FD", notify[1].to_string());
            }
            let err = cmd.exec();
            let _ = writeln!(
                io::stderr(),
                "rgetty: cannot exec {}: {}",
                cfg.login_program,
                err
            );
            libc::_exit(127);
        }
        pid
    };
    // 父进程：关闭写端，保留读端用于等待登录成功通知
    if notify[1] >= 0 {
        unsafe { libc::close(notify[1]) };
    }
    match wait_child_with_timeout(pid, notify[0], timeout_secs) {
        Some(0) => {} // shell 正常退出：立即重新提示
        Some(_) => {
            // 登录失败（非零退出）：延迟再提示，避免失败刷屏
            if cfg.failure_delay > 0 {
                std::thread::sleep(std::time::Duration::from_secs(cfg.failure_delay));
            }
        }
        None => {
            // 空闲超时：先换行（密码提示后无换行），再打印登出消息
            let _ = writeln!(io::stdout(), "\nsession timed out, logging out");
        }
    }
}

/// 等待子进程退出（轮询 WNOHANG + 监控 stdin 输入活动）。
/// 空闲计时从"登录成功"开始：先等 `notify_fd`（rlogin exec shell 前写入的通知，
/// -1 表示无通知机制，直接开始计时）；之后登录会话持续无输入达到 `idle_secs`
/// 则终止子进程并返回 None；有输入活动（用户敲键等）会刷新计时。
/// 返回 Some(退出码) 表示子进程已退出（信号杀死按失败码 1 处理）。
fn wait_child_with_timeout(
    pid: libc::pid_t,
    notify_fd: i32,
    idle_secs: Option<u64>,
) -> Option<i32> {
    let Some(idle) = idle_secs else {
        // 无超时：循环等待退出
        loop {
            if let Some(code) = wait_child_exit(pid) {
                return Some(code);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };

    // 阶段 1：等待登录成功通知（登录过程不计入空闲时间）
    if notify_fd >= 0 {
        loop {
            if let Some(code) = wait_child_exit(pid) {
                return Some(code);
            }
            let mut fds = [libc::pollfd {
                fd: notify_fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, 200) };
            if n > 0 {
                // 读通知（1 字节）；EOF（rlogin 未通知即退出/exec 失败）则继续轮询
                let mut b = [0u8; 1];
                let r = unsafe { libc::read(notify_fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
                if r == 1 {
                    break; // 登录成功，开始空闲计时
                }
            }
        }
    }

    // 阶段 2：空闲计时循环
    let mut last_active = std::time::Instant::now();
    loop {
        if let Some(code) = wait_child_exit(pid) {
            return Some(code);
        }

        let now = std::time::Instant::now();
        let deadline = last_active + std::time::Duration::from_secs(idle);
        if now >= deadline {
            // 空闲超时：终止整个进程组（子进程 setsid 后其 pid 即 pgid），
            // 可连带清理 shell 派生的后台进程；组不存在则回退单进程。
            terminate_session(pid, libc::SIGTERM);
            let kill_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                if wait_child_exit(pid).is_some() {
                    break;
                }
                if std::time::Instant::now() >= kill_deadline {
                    terminate_session(pid, libc::SIGKILL);
                    let _ = unsafe { libc::waitpid(pid, &mut std::mem::zeroed(), 0) };
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            return None;
        }

        // 监控终端输入：仅当存在真实输入字节（FIONREAD > 0）才视为活动并刷新计时，
        // 数据留给 shell 读取；EOF（poll 可读但无字节）不刷新。
        let remaining = (deadline - now).as_millis().min(i32::MAX as u128) as i32;
        let mut fds = [libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        }];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, remaining) };
        if n > 0 && fds[0].revents & libc::POLLIN != 0 {
            let mut avail: libc::c_int = 0;
            let has_input = unsafe { libc::ioctl(libc::STDIN_FILENO, libc::FIONREAD, &mut avail) }
                == 0
                && avail > 0;
            if has_input {
                last_active = std::time::Instant::now();
            }
        }
        // n == 0：poll 超时，循环顶部会触发空闲超时；n < 0：短暂重试
    }
}

/// 轮询等待子进程退出（WNOHANG）；已退出返回 Some(退出码)，未退出返回 None。
fn wait_child_exit(pid: libc::pid_t) -> Option<i32> {
    let mut status: libc::c_int = 0;
    let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if r == pid {
        return Some(if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            1
        });
    }
    if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
        return Some(1); // waitpid 出错，按失败处理
    }
    None
}

/// 向会话进程组发送信号；进程组不存在（未 setsid 的测试子进程等）时回退单进程。
fn terminate_session(pid: libc::pid_t, sig: i32) {
    if unsafe { libc::kill(-pid, sig) } != 0 {
        unsafe { libc::kill(pid, sig) };
    }
}

/// 读取一行用户名（登录提示阶段不超时）；EOF 返回 None。
fn read_username() -> Option<String> {
    let mut line = String::new();
    let n = io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if n == 0 { None } else { Some(line) }
}

/// 规范化用户名：去掉首尾空白，限制长度（防超长用户名撑爆 exec argv）。
fn normalize_username(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.chars().take(MAX_USERNAME_LEN).collect())
    }
}

/// 用户名最大长度（超过则截断）。
const MAX_USERNAME_LEN: usize = 64;

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
    fn parse_args_timeout_zero_means_no_timeout() {
        // -t 0 表示不超时，避免 0 秒超时导致无限重试刷屏
        let opts = parse_args(&["-t".to_string(), "0".to_string()]).unwrap();
        assert_eq!(opts.timeout_secs, None);
    }

    #[test]
    fn wait_child_returns_exit_code() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe { libc::_exit(42) };
        }
        assert_eq!(wait_child_with_timeout(pid, -1, None), Some(42));
    }

    #[test]
    fn wait_child_timeout_terminates() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            std::thread::sleep(std::time::Duration::from_secs(10));
            unsafe { libc::_exit(0) };
        }
        // 1 秒超时：子进程被终止，返回 None
        assert_eq!(wait_child_with_timeout(pid, -1, Some(1)), None);
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
    fn normalize_username_truncates_long() {
        let long = "u".repeat(200);
        let name = normalize_username(&long).unwrap();
        assert_eq!(name.chars().count(), MAX_USERNAME_LEN);
    }

    #[test]
    fn normalize_username_empty() {
        assert_eq!(normalize_username(""), None);
        assert_eq!(normalize_username("   \n"), None);
        assert_eq!(normalize_username("\r\n"), None);
    }
}
