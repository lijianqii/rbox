//! 执行器：执行命令列表、管道、重定向、外部命令查找。
//!
//! 前台进程 SIGINT 转发：shell 在等待前台子进程期间，如果收到 Ctrl-C
//! （SIGINT），会将 SIGINT 转发给所有子进程的进程组，实现中断当前正在
//! 运行的程序而不退出 shell。

use super::builtin::{BuiltinResult, try_builtin};
use super::expander::expand_history;
use super::expander::expand_pipeline;
use super::parser::build_command_list;
use super::reader::set_isig;
use super::tokenizer::tokenize;
use super::types::*;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

/// 以覆盖或追加方式打开文件用于重定向。
fn open_redirect(path: &str, append: bool) -> Option<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    let result = if append {
        opts.create(true).append(true).open(path)
    } else {
        opts.create(true).write(true).truncate(true).open(path)
    };
    match result {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("shell: {}: {}", path, e);
            None
        }
    }
}

/// 当前前台子进程组 ID（0 表示无前台进程在运行）。
/// SIGINT 处理器读取此值以转发信号。
static FOREGROUND_PGID: AtomicI32 = AtomicI32::new(0);

/// 注册 SIGINT 处理器：转发给前台进程组。
/// 在 shell 启动时调用一次。
/// raw 模式下 ISIG 已关闭，Ctrl-C 不产生 SIGINT 信号；
/// 此 handler 作为管道模式的后备（管道模式下 ISIG 仍然开启）。
pub fn install_sigint_handler() {
    extern "C" fn handle_sigint(_sig: i32) {
        let pgid = FOREGROUND_PGID.load(Ordering::Relaxed);
        if pgid > 0 {
            unsafe {
                libc::kill(-pgid, libc::SIGINT);
            }
        }
    }
    unsafe {
        libc::signal(libc::SIGINT, handle_sigint as *const () as usize);
    }
}

/// 执行一行命令。返回退出码。exit_fn 用于 `exit` 内置命令。
pub fn execute_line<F>(line: &str, last_rc: &mut i32, history: &[String], exit_fn: F) -> i32
where
    F: Fn(i32),
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
                BuiltinResult::Done => {
                    continue;
                }
                BuiltinResult::NotBuiltin => {}
            }
        }

        *last_rc = execute_pipeline(&expanded);
    }

    *last_rc
}

/// 执行一条管道（可能含多条 SimpleCmd）。
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
        command.args(&extra_args);
        command.args(&cmd.argv[1..]);

        // 前台进程：创建独立进程组 + 恢复 SIGINT 默认处理
        // （子进程继承了 shell 的 SIGINT handler，不恢复的话收到信号不会退出）
        if !pipeline.background {
            #[cfg(unix)]
            unsafe {
                command.pre_exec(|| {
                    libc::setpgid(0, 0);
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    libc::signal(libc::SIGTSTP, libc::SIG_DFL);
                    Ok(())
                });
            }
        }

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
        }

        // stdout
        if let Some(ref f) = cmd.stdout_file {
            match open_redirect(f, cmd.append) {
                Some(file) => {
                    command.stdout(Stdio::from(file));
                }
                None => return 1,
            }
        } else if i < ncmds - 1 {
            command.stdout(Stdio::piped());
        }

        // stderr
        if let Some(ref f) = cmd.stderr_file {
            match open_redirect(f, cmd.append_err) {
                Some(file) => {
                    command.stderr(Stdio::from(file));
                }
                None => return 1,
            }
        }

        // 管道中间命令的 stdin 来自前一个命令的 stdout
        if i > 0
            && cmd.stdin_file.is_none()
            && let Some(prev) = children.last_mut()
            && let Some(stdout) = prev.stdout.take()
        {
            command.stdin(Stdio::from(stdout));
        }

        match command.spawn() {
            Ok(child) => children.push(child),
            Err(e) => {
                eprintln!("shell: {}: {}", cmd.argv[0], e);
                // 关闭未消费的 pipe stdout，避免 fd 泄漏
                for child in &mut children {
                    child.stdout = None;
                }
                return 127;
            }
        }
    }

    // 后台运行：不等待
    if pipeline.background {
        return 0;
    }

    // 设置前台进程组：用第一个子进程的 pid 作为 pgid
    if let Some(first) = children.first() {
        let pgid = first.id() as i32;
        FOREGROUND_PGID.store(pgid, Ordering::Relaxed);
    }

    // 前台等待期间：
    // 1. 屏蔽 SIGCHLD —— 避免 SIGCHLD 处理器用 waitpid(-1) 抢收前台子进程，
    //    导致 child.wait() 返回 ECHILD、退出码被误判为 1。
    // 2. 开启 ISIG —— 让 Ctrl-C 产生 SIGINT 信号，由 SIGINT 处理器转发给
    //    前台进程组（替代原 stdin 监控线程 + TIOCSTI 推回方案，消除丢字符）。
    let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut sigset);
        libc::sigaddset(&mut sigset, libc::SIGCHLD);
        libc::pthread_sigmask(libc::SIG_BLOCK, &sigset, std::ptr::null_mut());
    }
    set_isig(true);

    // 等待所有子进程，返回最后一个的退出码
    let mut last_code = 0;
    for child in &mut children {
        match child.wait() {
            Ok(status) => last_code = status.code().unwrap_or(1),
            Err(_) => last_code = 1,
        }
    }

    set_isig(false);

    // 收割等待期间累积的后台僵尸（SIGCHLD 被屏蔽，处理器未执行）
    unsafe {
        loop {
            let pid = libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG);
            if pid <= 0 {
                break;
            }
        }
    }

    // 解除 SIGCHLD 屏蔽
    unsafe {
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &sigset, std::ptr::null_mut());
    }

    // 清除前台进程组标记
    FOREGROUND_PGID.store(0, Ordering::Relaxed);

    // 如果是被 SIGINT 中断的，打印换行使提示符对齐
    if last_code == 130 {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\n");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    last_code
}

/// 注册 SIGCHLD 处理器：自动回收后台僵尸子进程。
/// 前台命令等待期间主线程用 pthread_sigmask 屏蔽 SIGCHLD，此时处理器不执行，
/// 前台子进程由 child.wait() 收割，后台僵尸在等待结束后统一回收。
pub fn install_sigchld_handler() {
    extern "C" fn handle_sigchld(_sig: i32) {
        // 非阻塞 waitpid 回收所有已终止的子进程
        loop {
            let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
        }
    }
    unsafe {
        libc::signal(libc::SIGCHLD, handle_sigchld as *const () as usize);
    }
}

/// 命令查找：含 `/` 按字面路径，否则在 PATH 下查找。
/// 查找失败时回退到 rbox 内置 applet（`rbox <cmd>`）。
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

    // 回退：rbox 内置 applet
    let rbox_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/bin/rbox".to_string());
    (rbox_path, vec![cmd.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_redirect_creates_new_file() {
        let path = "/tmp/rbox_test_redirect_new";
        let _ = std::fs::remove_file(path);
        let f = open_redirect(path, false);
        assert!(f.is_some());
        assert!(std::path::Path::new(path).exists());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_redirect_append_mode() {
        let path = "/tmp/rbox_test_redirect_append";
        let _ = std::fs::remove_file(path);
        // First write
        let _ = open_redirect(path, false);
        // Append should also work
        let f = open_redirect(path, true);
        assert!(f.is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_redirect_fails_for_invalid_path() {
        // Directory as target -> error
        let f = open_redirect("/tmp", false);
        assert!(f.is_none());
    }

    #[test]
    fn resolve_command_finds_builtin_echo() {
        // echo should be found via PATH (coreutils on host or rbox fallback)
        let (program, _args) = resolve_command("echo");
        // Should resolve to some path containing "echo" or rbox
        assert!(
            program.contains("echo") || program.contains("rbox"),
            "expected echo or rbox, got: {}",
            program
        );
    }

    #[test]
    fn resolve_command_fallback_for_unknown() {
        // Unknown command -> rbox fallback
        let (program, args) = resolve_command("nonexistent_cmd_xyz");
        assert!(program.contains("rbox") || program.contains("cargo"));
        assert_eq!(args, vec!["nonexistent_cmd_xyz"]);
    }
}
