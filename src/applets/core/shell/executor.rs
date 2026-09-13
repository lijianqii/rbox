//! 执行器：执行命令列表、管道、重定向、外部命令查找。
//!
//! 前台进程 SIGINT 转发：shell 在等待前台子进程期间，如果收到 Ctrl-C
//! （SIGINT），会将 SIGINT 转发给所有子进程的进程组，实现中断当前正在
//! 运行的程序而不退出 shell。

use super::alias;
use super::builtin::{BuiltinResult, is_builtin, try_builtin};
use super::expander::expand_history;
use super::expander::expand_pipeline;
use super::jobs;
use super::parser::build_command_list;
use super::tokenizer::tokenize;
use super::types::*;
use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

/// 前台命令等待期间，Ctrl-C 监控线程读到的非 0x03 字节缓存于此。
/// REPL 主循环读取 stdin 前先消费本队列，避免监控线程与主线程
/// 并发读 stdin 导致字节错位/丢失（TIOCSTI 推回队尾会乱序，已弃用）。
static PENDING_STDIN: OnceLock<Mutex<VecDeque<u8>>> = OnceLock::new();

pub(crate) fn pending_stdin() -> &'static Mutex<VecDeque<u8>> {
    PENDING_STDIN.get_or_init(|| Mutex::new(VecDeque::new()))
}

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
    // 别名展开（命令位置首词）与命令替换 $(...)
    let expanded_line = alias::expand_alias(&expanded_line);
    let expanded_line = expand_command_subst(&expanded_line);
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
            // 内置命令：应用重定向（pwd > file 等），与外部命令行为一致
            let argv0 = expanded.cmds[0]
                .argv
                .first()
                .map(String::as_str)
                .unwrap_or("");
            if is_builtin(argv0) {
                match apply_builtin_redirects(&expanded.cmds[0]) {
                    Err(code) => {
                        *last_rc = code;
                        continue;
                    }
                    Ok(_guard) => match try_builtin(&expanded.cmds[0], last_rc, history) {
                        BuiltinResult::Exit => {
                            exit_fn(*last_rc);
                        }
                        BuiltinResult::Done => {
                            continue;
                        }
                        BuiltinResult::NotBuiltin => {}
                    },
                }
            }
        }

        *last_rc = execute_pipeline(&expanded, line);
    }

    *last_rc
}

/// 内置命令重定向的恢复句柄：Drop 时恢复原标准流并关闭保存的 fd。
struct BuiltinRedirectGuard {
    saved_in: Option<i32>,
    saved_out: Option<i32>,
    saved_err: Option<i32>,
    /// (保存的 fd, 被复制的目标 fd)：恢复时 dup2(saved, from)。
    saved_dups: Vec<(i32, i32)>,
}

impl Drop for BuiltinRedirectGuard {
    fn drop(&mut self) {
        use std::io::Write;
        // 先把 Rust 侧缓冲写出，再恢复 fd，避免输出落到错误的目标
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        unsafe {
            for (saved, from) in self.saved_dups.iter().rev() {
                libc::dup2(*saved, *from);
                libc::close(*saved);
            }
            self.saved_dups.clear();
            if let Some(fd) = self.saved_out.take() {
                libc::dup2(fd, libc::STDOUT_FILENO);
                libc::close(fd);
            }
            if let Some(fd) = self.saved_err.take() {
                libc::dup2(fd, libc::STDERR_FILENO);
                libc::close(fd);
            }
            if let Some(fd) = self.saved_in.take() {
                libc::dup2(fd, libc::STDIN_FILENO);
                libc::close(fd);
            }
        }
    }
}

/// 为内置命令应用重定向：先把所有目标文件打开成功，再用 `dup2` 临时替换
/// 标准流（单线程时刻执行，无并发读取）；失败返回错误码，不修改任何 fd。
/// 返回的 guard 在 Drop 时恢复原始标准流。
fn apply_builtin_redirects(cmd: &SimpleCmd) -> Result<Option<BuiltinRedirectGuard>, i32> {
    if cmd.stdin_file.is_none()
        && cmd.stdout_file.is_none()
        && cmd.stderr_file.is_none()
        && cmd.dup_fds.is_empty()
    {
        return Ok(None);
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();

    let in_file = match &cmd.stdin_file {
        Some(f) => match std::fs::File::open(f) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("shell: {}: {}", f, e);
                return Err(1);
            }
        },
        None => None,
    };
    let out_file = match &cmd.stdout_file {
        Some(f) => match open_redirect(f, cmd.append) {
            Some(f) => Some(f),
            None => return Err(1),
        },
        None => None,
    };
    let err_file = match &cmd.stderr_file {
        Some(f) => match open_redirect(f, cmd.append_err) {
            Some(f) => Some(f),
            None => return Err(1),
        },
        None => None,
    };

    let mut guard = BuiltinRedirectGuard {
        saved_in: None,
        saved_out: None,
        saved_err: None,
        saved_dups: Vec::new(),
    };
    unsafe {
        if let Some(f) = in_file {
            let saved = libc::dup(libc::STDIN_FILENO);
            if saved >= 0 && libc::dup2(f.as_raw_fd(), libc::STDIN_FILENO) >= 0 {
                guard.saved_in = Some(saved);
            } else if saved >= 0 {
                libc::close(saved);
            }
        }
        if let Some(f) = out_file {
            let saved = libc::dup(libc::STDOUT_FILENO);
            if saved >= 0 && libc::dup2(f.as_raw_fd(), libc::STDOUT_FILENO) >= 0 {
                guard.saved_out = Some(saved);
            } else if saved >= 0 {
                libc::close(saved);
            }
        }
        if let Some(f) = err_file {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved >= 0 && libc::dup2(f.as_raw_fd(), libc::STDERR_FILENO) >= 0 {
                guard.saved_err = Some(saved);
            } else if saved >= 0 {
                libc::close(saved);
            }
        }
        // 描述符复制在文件重定向之后应用（与 `> file 2>&1` 语义一致）
        for (from, to) in &cmd.dup_fds {
            let from = *from as i32;
            let to = *to as i32;
            let saved = libc::dup(from);
            if saved >= 0 && libc::dup2(to, from) >= 0 {
                guard.saved_dups.push((saved, from));
            } else if saved >= 0 {
                libc::close(saved);
            }
        }
    }
    Ok(Some(guard))
}

// ─── 命令替换 $(...) ──────────────────────────────────

/// 展开行内命令替换 `$(cmd)`（单引号内不展开，双引号/引号外展开）。
/// 先递归展开内层，再执行内层命令并以前端 stdout（去除末尾换行）替换。
/// 输出不进行二次语法解析（仅按分词器规则参与分词，与 POSIX 相近）。
pub fn expand_command_subst(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
            if c == b'\'' {
                in_squote = false;
            }
            continue;
        }
        if in_dquote && c == b'\\' {
            // 双引号内转义：原样复制反斜杠与下一字符
            let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
            if i < bytes.len() {
                let ch2 = line[i..].chars().next().unwrap_or('\u{fffd}');
                out.push(ch2);
                i += ch2.len_utf8();
            }
            continue;
        }
        match c {
            b'\'' => {
                in_squote = true;
                out.push('\'');
                i += 1;
            }
            b'"' => {
                in_dquote = !in_dquote;
                out.push('"');
                i += 1;
            }
            b'$' if i + 1 < bytes.len()
                && bytes[i + 1] == b'('
                && bytes.get(i + 2) != Some(&b'(') =>
            {
                if let Some((inner, close)) = find_subst_end(line, i + 2) {
                    let inner = expand_command_subst(&inner);
                    let output = capture_output(&inner).unwrap_or_default();
                    out.push_str(output.trim_end_matches('\n'));
                    i = close + 1;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// 从 `$(` 之后的位置找到匹配的 `)`，返回 (内部文本, `)` 的字节索引)。
/// 处理嵌套括号与引号；未闭合返回 None。
fn find_subst_end(line: &str, start: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let mut depth = 1;
    let mut i = start;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match c {
                b'"' => in_dquote = false,
                b'\\' => i += 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((line[start..i].to_string(), i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 执行一行命令并捕获其 stdout（命令替换用）。
/// 仅支持外部命令/管道（内置命令需经 rbox applet 兼容路径）。
pub fn capture_output(line: &str) -> Option<String> {
    let tokens = tokenize(line);
    let cmd_list = build_command_list(&tokens).ok()?;
    let mut last_rc = 0;
    let mut output = String::new();
    for seg in &cmd_list.segments {
        match seg.connector {
            Connector::AndIf if last_rc != 0 => continue,
            Connector::OrIf if last_rc == 0 => continue,
            _ => {}
        }
        let expanded = expand_pipeline(&seg.pipeline, last_rc).ok()?;
        if expanded.cmds.is_empty() {
            continue;
        }
        let (rc, out) = capture_pipeline(&expanded)?;
        last_rc = rc;
        output = out;
    }
    Some(output)
}

/// 执行管道并把最后一条命令的 stdout 读入字符串；返回 (退出码, 输出)。
fn capture_pipeline(pipeline: &Pipeline) -> Option<(i32, String)> {
    // spawn 到等待结束全程屏蔽 SIGCHLD（同 execute_pipeline）
    let _sigchld = SigchldGuard::block();
    let mut children: Vec<Child> = Vec::new();
    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        if cmd.argv.is_empty() {
            continue;
        }
        let (program, extra_args) = resolve_command(&cmd.argv[0]);
        let mut command = Command::new(program);
        command.args(&extra_args);
        command.args(&cmd.argv[1..]);

        let dups = cmd.dup_fds.clone();
        if !dups.is_empty() {
            unsafe {
                command.pre_exec(move || {
                    for (from, to) in &dups {
                        libc::dup2(*to as i32, *from as i32);
                    }
                    Ok(())
                });
            }
        }

        if let Some(ref f) = cmd.stdin_file {
            match std::fs::File::open(f) {
                Ok(file) => {
                    command.stdin(Stdio::from(file));
                }
                Err(e) => {
                    eprintln!("shell: {}: {}", f, e);
                    cleanup_spawned_children(&mut children);
                    return None;
                }
            }
        }
        match &cmd.stdout_file {
            Some(f) => match open_redirect(f, cmd.append) {
                Some(file) => {
                    command.stdout(Stdio::from(file));
                }
                None => {
                    cleanup_spawned_children(&mut children);
                    return None;
                }
            },
            None => {
                command.stdout(Stdio::piped());
            }
        }
        if let Some(ref f) = cmd.stderr_file {
            match open_redirect(f, cmd.append_err) {
                Some(file) => {
                    command.stderr(Stdio::from(file));
                }
                None => {
                    cleanup_spawned_children(&mut children);
                    return None;
                }
            }
        }
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
                cleanup_spawned_children(&mut children);
                return None;
            }
        }
    }

    // 最后一条命令的 stdout 在独立线程读取，避免管道缓冲满导致死锁
    let reader = children
        .last_mut()
        .and_then(|c| c.stdout.take())
        .map(|mut out| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = std::io::Read::read_to_string(&mut out, &mut s);
                s
            })
        });

    let mut last_code = 0;
    for child in &mut children {
        match child.wait() {
            Ok(status) => {
                last_code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            }
            Err(_) => last_code = 1,
        }
    }
    let output = match reader {
        Some(h) => h.join().unwrap_or_default(),
        None => String::new(),
    };

    Some((last_code, output))
}

/// 屏蔽 SIGCHLD 的 RAII guard（Drop 时恢复原掩码）。
/// `Command::spawn()` 在 exec 失败时会在内部 wait 子进程，此时 SIGCHLD
/// 处理器若抢先用 waitpid(-1) 收割，会让 std 的 `Child::wait()` 遇 ECHILD
/// 触发 `wait() should either return Ok or panic` 断言崩溃。
/// 覆盖整个 spawn + 等待周期可消除该竞态。
struct SigchldGuard {
    old: libc::sigset_t,
}

impl SigchldGuard {
    fn block() -> Self {
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGCHLD);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
        }
        SigchldGuard { old }
    }
}

impl Drop for SigchldGuard {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.old, std::ptr::null_mut());
        }
    }
}

/// 等待指定 pid：返回 (退出码, 是否被挂起)。
/// 使用 `WUNTRACED` 感知 Ctrl-Z 挂起（前台进程组整体停止）。
fn wait_child_pid(pid: i32) -> (i32, bool) {
    loop {
        let mut status: libc::c_int = 0;
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if r < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return (1, false); // ECHILD：已被 SIGCHLD 处理器收割
        }
        if libc::WIFSTOPPED(status) {
            return (128 + libc::WSTOPSIG(status), true);
        }
        if libc::WIFSIGNALED(status) {
            return (128 + libc::WTERMSIG(status), false);
        }
        return (libc::WEXITSTATUS(status), false);
    }
}

/// 执行一条管道（可能含多条 SimpleCmd）。`cmdline` 为原始命令行（作业控制显示用）。
fn execute_pipeline(pipeline: &Pipeline, cmdline: &str) -> i32 {
    if pipeline.cmds.is_empty() {
        return 0;
    }
    // spawn 到等待结束全程屏蔽 SIGCHLD（见 SigchldGuard 注释）
    let _sigchld = SigchldGuard::block();

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

        // 所有子进程放入独立进程组（前后台统一）：
        // - 前台：便于 Ctrl-C/Ctrl-Z 按组转发信号；
        // - 后台：作业控制需要独立 pgid（fg/bg 按组 SIGCONT）。
        // `N>&M` 描述符复制在 stdio 设置完成后（pre_exec）应用，
        // 保证 `> file 2>&1` 中 stderr 指向已打开的文件。
        let dups = cmd.dup_fds.clone();
        #[cfg(unix)]
        unsafe {
            command.pre_exec(move || {
                libc::setpgid(0, 0);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                libc::signal(libc::SIGTSTP, libc::SIG_DFL);
                for (from, to) in &dups {
                    libc::dup2(*to as i32, *from as i32);
                }
                Ok(())
            });
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
                cleanup_spawned_children(&mut children);
                return 127;
            }
        }
    }

    // 后台运行：登记作业并立即返回（打印 [id] pgid，与 bash 风格一致）
    if pipeline.background {
        if let Some(first) = children.first() {
            let pgid = first.id() as i32;
            let id = jobs::add_job(pgid, cmdline, jobs::JobState::Running);
            if id > 0 {
                println!("[{}] {}", id, pgid);
            }
        }
        return 0;
    }

    // 设置前台进程组：用第一个子进程的 pid 作为 pgid
    if let Some(first) = children.first() {
        let pgid = first.id() as i32;
        FOREGROUND_PGID.store(pgid, Ordering::Relaxed);
    }

    // 前台等待期间：
    // 1. SIGCHLD 已在 spawn 前屏蔽（SigchldGuard），避免处理器用 waitpid(-1)
    //    抢收前台子进程导致 waitpid(pid) 返回 ECHILD。

    // 前台等待期间，启动一个线程监听 stdin 的 Ctrl-C（0x03 字节）。
    // raw 模式下 ISIG 已关闭，Ctrl-C 不产生信号，而是作为 0x03 字节到达。
    // shell 主线程在 wait() 中阻塞，无法读 stdin，所以需要单独线程。
    let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor_stop = stop_flag.clone();
    let monitor = std::thread::spawn(move || {
        let mut buf = [0u8; 1];
        loop {
            if monitor_stop.load(Ordering::Relaxed) {
                return;
            }
            // 非阻塞 read，避免线程无法退出
            let stdin_fd = std::io::stdin().as_raw_fd();
            let flags = unsafe { libc::fcntl(stdin_fd, libc::F_GETFL) };
            unsafe { libc::fcntl(stdin_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
            let n = unsafe { libc::read(stdin_fd, buf.as_mut_ptr() as *mut _, 1) };
            // 恢复阻塞
            unsafe { libc::fcntl(stdin_fd, libc::F_SETFL, flags) };

            if n == 1 {
                if buf[0] == 0x03 {
                    // Ctrl-C：向子进程组发送 SIGINT
                    let pgid = FOREGROUND_PGID.load(Ordering::Relaxed);
                    if pgid > 0 {
                        unsafe {
                            libc::kill(-pgid, libc::SIGINT);
                        }
                    }
                    return;
                } else if buf[0] == 0x1a {
                    // Ctrl-Z：向子进程组发送 SIGTSTP（挂起，由 WUNTRACED 感知）
                    let pgid = FOREGROUND_PGID.load(Ordering::Relaxed);
                    if pgid > 0 {
                        unsafe {
                            libc::kill(-pgid, libc::SIGTSTP);
                        }
                    }
                    return;
                } else {
                    // 非 Ctrl-C 字节：缓存到 pending 队列，主循环随后消费。
                    // （TIOCSTI 推回会追加到 tty 输入队列队尾，与主线程并发
                    //   读 stdin 时乱序/错位，改为共享队列保序）
                    pending_stdin().lock().unwrap().push_back(buf[0]);
                }
            } else {
                // 没有数据，短暂休眠后重试
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    });

    // 等待所有子进程，返回最后一个的退出码；检测到挂起则登记作业
    let mut last_code = 0;
    for child in &mut children {
        let pid = child.id() as i32;
        let (code, stopped) = wait_child_pid(pid);
        last_code = code;
        if stopped {
            let id = jobs::add_job(pid, cmdline, jobs::JobState::Stopped);
            if id > 0 {
                println!("[{}]+ Stopped  {}", id, cmdline);
            }
            break;
        }
    }

    // 停止 stdin 监控线程
    stop_flag.store(true, Ordering::Relaxed);
    let _ = monitor.join();

    // 收割等待期间累积的后台僵尸（SIGCHLD 被屏蔽，处理器未执行）
    unsafe {
        loop {
            let pid = libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG);
            if pid <= 0 {
                break;
            }
        }
    }

    // SIGCHLD 由 SigchldGuard 在函数返回时恢复

    // 清除前台进程组标记
    FOREGROUND_PGID.store(0, Ordering::Relaxed);

    // 如果是被 SIGINT 中断的，打印换行使提示符对齐
    if last_code == 130 {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\n");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    last_code
}

/// 管道中途 spawn 失败时，终止并回收已经成功启动的子进程，避免残留进程/僵尸。
/// 用原始 `waitpid` 而非 `Child::wait()`：SIGCHLD 处理器可能已并发收割子进程，
/// `Child::wait()` 遇 ECHILD 会 panic（std 的断言）；原始调用忽略该错误。
fn cleanup_spawned_children(children: &mut [Child]) {
    // 清理期间屏蔽 SIGCHLD，避免处理器抢收后 waitpid 返回 ECHILD 的竞态
    let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
    let mut oldset: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut sigset);
        libc::sigaddset(&mut sigset, libc::SIGCHLD);
        libc::pthread_sigmask(libc::SIG_BLOCK, &sigset, &mut oldset);
    }
    for child in children {
        // 前台子进程通过 pre_exec 创建了独立进程组；尽量整组清理。
        // 后台子进程可能仍在 shell 进程组中，此时负 pid kill 会失败，回退 child.kill()。
        let pid = child.id() as i32;
        let rc = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if rc != 0 {
            let _ = child.kill();
        }
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
    unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, &oldset, std::ptr::null_mut());
    }
}

/// 注册 SIGCHLD 处理器：自动回收后台僵尸子进程。
/// 前台命令等待期间主线程用 pthread_sigmask 屏蔽 SIGCHLD，此时处理器不执行，
/// 前台子进程由 `wait_child_pid` 收割，后台僵尸在等待结束后统一回收。
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

/// 检查路径是否为可执行文件（`X_OK`）。
/// 仅 `is_file()` 会把 PATH 中不可执行的同名文件当成命中，导致 spawn 失败
/// 而不继续搜索后续目录。
fn is_executable(path: &str) -> bool {
    use std::ffi::CString;
    let Ok(c) = CString::new(path) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// 命令查找：含 `/` 按字面路径，否则在 PATH 下查找。
/// 查找失败时回退到 rbox 内置 applet（`rbox <cmd>`）。
fn resolve_command(cmd: &str) -> (String, Vec<String>) {
    if cmd.contains('/') {
        return (cmd.to_string(), Vec::new());
    }

    // 在 PATH 下查找（仅命中可执行文件）
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            let full = format!("{}/{}", dir, cmd);
            if is_executable(&full) {
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

    #[test]
    fn builtin_redirects_none_for_plain_cmd() {
        let cmd = SimpleCmd {
            argv: vec!["pwd".into()],
            ..Default::default()
        };
        assert!(apply_builtin_redirects(&cmd).unwrap().is_none());
    }

    #[test]
    fn builtin_redirects_error_for_unopenable_file() {
        // 目标为目录：打开失败，应返回错误码且不修改标准流
        let cmd = SimpleCmd {
            argv: vec!["pwd".into()],
            stdout_file: Some("/tmp".into()),
            ..Default::default()
        };
        assert_eq!(apply_builtin_redirects(&cmd).err(), Some(1));
    }

    #[test]
    fn is_executable_checks_permission_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = format!("/tmp/rbox_exec_test_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let plain = format!("{}/plain", dir);
        let exec = format!("{}/exec", dir);
        std::fs::write(&plain, "x").unwrap();
        std::fs::write(&exec, "x").unwrap();
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_executable(&plain));
        assert!(is_executable(&exec));
        assert!(!is_executable("/nonexistent_rbox_path"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_subst_end_matches_nested() {
        let line = "$(echo $(echo hi)) tail";
        let (inner, close) = find_subst_end(line, 2).unwrap();
        assert_eq!(inner, "echo $(echo hi)");
        assert_eq!(&line[close..close + 1], ")");
    }

    #[test]
    fn find_subst_end_unclosed_returns_none() {
        assert!(find_subst_end("$(echo hi", 2).is_none());
    }

    #[test]
    fn expand_command_subst_replaces_output() {
        let out = expand_command_subst("echo $(echo hi)");
        assert_eq!(out, "echo hi");
    }

    #[test]
    fn expand_command_subst_single_quotes_not_expanded() {
        let out = expand_command_subst("echo '$(echo hi)'");
        assert_eq!(out, "echo '$(echo hi)'");
    }

    #[test]
    fn cleanup_spawned_children_terminates() {
        let mut child = std::process::Command::new("sleep")
            .arg("100")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        cleanup_spawned_children(std::slice::from_mut(&mut child));
        // 已回收：kill(pid, 0) 应失败（ESRCH）
        let rc = unsafe { libc::kill(pid, 0) };
        assert_ne!(rc, 0, "进程应已终止并回收");
    }
}
