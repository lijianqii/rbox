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
use super::{compound, functions, params, script};
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
fn open_redirect(path: &str, append: bool, force: bool) -> Option<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    let result = if append {
        opts.create(true).append(true).open(path)
    } else if !force && crate::applets::core::shell::options::noclobber() {
        // set -C：不覆盖已存在文件
        opts.create_new(true).write(true).open(path)
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
pub fn execute_line(
    line: &str,
    last_rc: &mut i32,
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> i32 {
    // 历史扩展：!! -> 上一条命令，!n -> 第 n 条，!$ -> 上一条命令的最后一个参数
    let expanded_line = expand_history(line, history);
    // 别名展开（命令位置首词）、反引号与 $(...) 命令替换
    // POSIX：别名不在非交互式 shell 中展开
    let expanded_line = if super::options::interactive() {
        alias::expand_alias(&expanded_line)
    } else {
        expanded_line
    };
    let expanded_line = expand_backticks(&expanded_line);
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

    for (seg_idx, seg) in cmd_list.segments.iter().enumerate() {
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

        let mut expanded = match expand_pipeline(&seg.pipeline, *last_rc) {
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

        // 拆分命令前导赋值（VAR=val cmd / 纯赋值 VAR=val）
        if !split_assignments(&mut expanded.cmds, *last_rc) {
            *last_rc = 1;
            continue;
        }

        // nounset 违规：展开阶段发现未定义变量（由脚本驱动决定退出）
        if crate::applets::core::shell::options::nounset_violation() {
            *last_rc = 1;
            continue;
        }

        // xtrace：打印展开后的命令
        if crate::applets::core::shell::options::xtrace() {
            let mut parts: Vec<String> = expanded.cmds[0]
                .env
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            parts.extend(expanded.cmds[0].argv.iter().cloned());
            let ps4 = std::env::var("PS4").unwrap_or_else(|_| "+ ".to_string());
            eprintln!("{}{}", ps4, parts.join(" "));
        }

        // 函数调用：函数优先于内置与外部命令
        if expanded.cmds.len() == 1
            && !expanded.background
            && let Some(name) = expanded.cmds[0].argv.first()
            && functions::is_function(name)
        {
            let fargs: Vec<String> = expanded.cmds[0].argv[1..].to_vec();
            if let Some(rc) = functions::call(name, &fargs, history, &exit_fn) {
                *last_rc = rc;
                if let Some(f) = compound::take_flow() {
                    return f;
                }
                continue;
            }
        }

        // `eval` / `command` / `source` 需要递归执行，特殊处理（不进入内置分发）
        if expanded.cmds.len() == 1 && !expanded.background {
            let argv0 = expanded.cmds[0]
                .argv
                .first()
                .map(String::as_str)
                .unwrap_or("");
            if argv0 == "source" || argv0 == "." {
                let Some(path) = expanded.cmds[0].argv.get(1) else {
                    eprintln!("source: filename argument required");
                    *last_rc = 2;
                    continue;
                };
                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("source: {}: {}", path, e);
                        *last_rc = 1;
                        continue;
                    }
                };
                let with_args = expanded.cmds[0].argv.len() > 2;
                let saved = params::all();
                if with_args {
                    params::set(expanded.cmds[0].argv[2..].to_vec());
                }
                *last_rc = script::run_source(&content, history, &exit_fn, false);
                if with_args {
                    params::set(saved);
                }
                continue;
            }
            if argv0 == "eval" {
                let script = expanded.cmds[0].argv[1..].join(" ");
                if !script.trim().is_empty() {
                    *last_rc = execute_line(&script, last_rc, history, exit_fn);
                } else {
                    *last_rc = 0;
                }
                if crate::applets::core::shell::options::return_requested() {
                    return *last_rc;
                }
                continue;
            }
            if argv0 == "command" {
                let args = &expanded.cmds[0].argv[1..];
                if args.is_empty() {
                    *last_rc = 0;
                    continue;
                }
                if args[0] == "-v" || args[0] == "-V" {
                    let verbose = args[0] == "-V";
                    let mut rc = 0;
                    for name in &args[1..] {
                        match command_path(name) {
                            Some(p) => {
                                if verbose {
                                    println!("{} is {}", name, p);
                                } else {
                                    println!("{}", p);
                                }
                            }
                            None => {
                                if verbose {
                                    println!("{}: not found", name);
                                }
                                rc = 1;
                            }
                        }
                    }
                    *last_rc = rc;
                    continue;
                }
                if args[0] == "-p" {
                    // `command -p`：使用 POSIX 默认 PATH 查找并执行
                    let saved = std::env::var("PATH").ok();
                    // SAFETY: shell 单线程
                    unsafe {
                        std::env::set_var("PATH", "/sbin:/usr/sbin:/bin:/usr/bin");
                    }
                    let script = args[1..].join(" ");
                    *last_rc = execute_line(&script, last_rc, history, exit_fn);
                    match saved {
                        Some(p) => unsafe {
                            std::env::set_var("PATH", p);
                        },
                        None => unsafe {
                            std::env::remove_var("PATH");
                        },
                    }
                    continue;
                }
                let script = args.join(" ");
                *last_rc = execute_line(&script, last_rc, history, exit_fn);
                if crate::applets::core::shell::options::return_requested() {
                    return *last_rc;
                }
                continue;
            }
        }

        // 纯赋值（无命令）：写入 shell 环境，退出码 0
        if expanded.cmds.len() == 1
            && !expanded.background
            && expanded.cmds[0].argv.is_empty()
            && !expanded.cmds[0].env.is_empty()
        {
            for (k, v) in &expanded.cmds[0].env {
                // SAFETY: shell 单线程
                unsafe {
                    std::env::set_var(k, v);
                }
            }
            *last_rc = 0;
            continue;
        }

        if expanded.cmds.len() == 1 && !expanded.background {
            // 内置命令：应用重定向（pwd > file 等），与外部命令行为一致
            let argv0 = expanded.cmds[0]
                .argv
                .first()
                .map(String::as_str)
                .unwrap_or("");
            // 内置命令的 VAR=val 前缀：临时写入环境
            for (k, v) in &expanded.cmds[0].env {
                // SAFETY: shell 单线程
                unsafe {
                    std::env::set_var(k, v);
                }
            }
            if is_builtin(argv0) {
                match apply_redirects(&expanded.cmds[0]) {
                    Err(code) => {
                        *last_rc = code;
                        continue;
                    }
                    Ok(_guard) => match try_builtin(&expanded.cmds[0], last_rc, history) {
                        BuiltinResult::Exit => {
                            exit_fn(*last_rc);
                        }
                        BuiltinResult::Done => {
                            if let Some(f) = compound::take_flow() {
                                return f;
                            }
                            continue;
                        }
                        BuiltinResult::NotBuiltin => {}
                    },
                }
            }
        }

        *last_rc = execute_pipeline(&expanded, line);
        // return 请求：向上传播（source/函数帧消费）
        if crate::applets::core::shell::options::return_requested() {
            return *last_rc;
        }
        // break/continue 内置在特殊路径（eval/command/source）中触发的哨兵
        if let Some(f) = compound::take_flow() {
            return f;
        }
        // -e：非条件链段失败即请求退出（&&/|| 链豁免）
        if crate::applets::core::shell::options::errexit() && *last_rc != 0 {
            let next_cond = matches!(
                cmd_list.segments.get(seg_idx + 1).map(|s| s.connector),
                Some(Connector::AndIf) | Some(Connector::OrIf)
            );
            let cur_cond = matches!(seg.connector, Connector::AndIf | Connector::OrIf);
            if !next_cond && !cur_cond {
                crate::applets::core::shell::options::request_exit(*last_rc);
                return *last_rc;
            }
        }
    }

    *last_rc
}

/// 内置命令重定向的恢复句柄：Drop 时恢复原标准流并关闭保存的 fd。
pub(crate) struct BuiltinRedirectGuard {
    saved_in: Option<i32>,
    saved_out: Option<i32>,
    saved_err: Option<i32>,
    /// (保存的 fd, 被复制的目标 fd)：恢复时 dup2(saved, from)。
    saved_dups: Vec<(i32, i32)>,
    /// 被关闭的 fd（恢复时 dup2(saved, fd)）。
    saved_closes: Vec<(i32, i32)>,
    /// 本次是否重定向了 stdin（Drop 时清除全局标记）。
    stdin_redirect: bool,
}

impl Drop for BuiltinRedirectGuard {
    fn drop(&mut self) {
        use std::io::Write;
        if self.stdin_redirect {
            STDIN_REDIRECTED.store(false, Ordering::SeqCst);
        }
        // 先把 Rust 侧缓冲写出，再恢复 fd，避免输出落到错误的目标
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        // `exec` 无参数时要求永久重定向：关闭保存的 fd 但不恢复
        if PERSIST_REDIRECTS.swap(false, Ordering::SeqCst) {
            unsafe {
                for (saved, _) in self.saved_dups.drain(..) {
                    if saved >= 0 {
                        libc::close(saved);
                    }
                }
                if let Some(fd) = self.saved_out.take() {
                    libc::close(fd);
                }
                if let Some(fd) = self.saved_err.take() {
                    libc::close(fd);
                }
                if let Some(fd) = self.saved_in.take() {
                    libc::close(fd);
                }
            }
            return;
        }
        unsafe {
            for (saved, from) in self.saved_dups.iter().rev() {
                if *saved >= 0 {
                    libc::dup2(*saved, *from);
                    libc::close(*saved);
                } else {
                    libc::close(*from);
                }
            }
            self.saved_dups.clear();
            for (saved, fd) in self.saved_closes.iter().rev() {
                libc::dup2(*saved, *fd);
                libc::close(*saved);
            }
            self.saved_closes.clear();
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

/// `exec` 无参数时置位：让内置重定向 guard 不恢复（永久重定向）。
static PERSIST_REDIRECTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 内置命令执行期间 stdin 被重定向（`read x < file`）：此时不得消费 REPL 预读缓冲。
static STDIN_REDIRECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// stdin 是否处于内置重定向中。
pub(crate) fn stdin_redirected() -> bool {
    STDIN_REDIRECTED.load(Ordering::SeqCst)
}

/// 请求持久化内置重定向（供 `exec` 使用）。
pub(crate) fn persist_builtin_redirects() {
    PERSIST_REDIRECTS.store(true, Ordering::SeqCst);
}

/// 为内置命令应用重定向：先把所有目标文件打开成功，再用 `dup2` 临时替换
/// 标准流（单线程时刻执行，无并发读取）；失败返回错误码，不修改任何 fd。
/// 返回的 guard 在 Drop 时恢复原始标准流。
pub(crate) fn apply_redirects(cmd: &SimpleCmd) -> Result<Option<BuiltinRedirectGuard>, i32> {
    if cmd.stdin_file.is_none()
        && cmd.stdout_file.is_none()
        && cmd.stderr_file.is_none()
        && cmd.dup_fds.is_empty()
        && cmd.close_fds.is_empty()
        && cmd.rw_file.is_none()
        && cmd.fd_redirects.is_empty()
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
        Some(f) => match open_redirect(f, cmd.append, cmd.force) {
            Some(f) => Some(f),
            None => return Err(1),
        },
        None => None,
    };
    let err_file = match &cmd.stderr_file {
        Some(f) => match open_redirect(f, cmd.append_err, cmd.force) {
            Some(f) => Some(f),
            None => return Err(1),
        },
        None => None,
    };
    let rw_file = match &cmd.rw_file {
        Some(f) => match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(f)
        {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("shell: {}: {}", f, e);
                return Err(1);
            }
        },
        None => None,
    };
    let mut extra_files: Vec<(u8, std::fs::File)> = Vec::new();
    for r in &cmd.fd_redirects {
        let f = if r.input {
            std::fs::File::open(&r.path).ok()
        } else {
            open_redirect(&r.path, r.append, cmd.force)
        };
        match f {
            Some(f) => extra_files.push((r.fd, f)),
            None => {
                eprintln!("shell: {}: cannot open", r.path);
                return Err(1);
            }
        }
    }

    // 文件 fd 迁移到高位：目标 fd 关闭时 open 可能恰好占用目标 fd 本身，
    // 会使 dup2 成为空操作、并在 File drop 时关闭目标 fd
    let extra_files: Vec<(u8, std::fs::File)> = extra_files
        .into_iter()
        .map(|(fd, f)| {
            use std::os::unix::io::FromRawFd;
            let new_fd = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
            if new_fd >= 0 {
                (fd, unsafe { std::fs::File::from_raw_fd(new_fd) })
            } else {
                (fd, f)
            }
        })
        .collect();

    let mut guard = BuiltinRedirectGuard {
        saved_in: None,
        saved_out: None,
        saved_err: None,
        saved_dups: Vec::new(),
        saved_closes: Vec::new(),
        stdin_redirect: false,
    };
    unsafe {
        if let Some(f) = in_file {
            let saved = libc::dup(libc::STDIN_FILENO);
            if saved >= 0 && libc::dup2(f.as_raw_fd(), libc::STDIN_FILENO) >= 0 {
                guard.saved_in = Some(saved);
                guard.stdin_redirect = true;
                STDIN_REDIRECTED.store(true, Ordering::SeqCst);
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
        if let Some(f) = rw_file {
            // `<>`：以 O_RDWR 打开并仅复制到 stdin（POSIX 语义）
            let saved_in = libc::dup(libc::STDIN_FILENO);
            if saved_in >= 0 && libc::dup2(f.as_raw_fd(), libc::STDIN_FILENO) >= 0 {
                guard.saved_in = Some(saved_in);
            } else if saved_in >= 0 {
                libc::close(saved_in);
            }
        }
        for (fd, f) in &extra_files {
            let fd = *fd as i32;
            let saved = libc::dup(fd);
            // saved < 0 表示目标 fd 原先关闭：Drop 时恢复为关闭
            if libc::dup2(f.as_raw_fd(), fd) >= 0 {
                guard.saved_dups.push((saved, fd));
            } else if saved >= 0 {
                libc::close(saved);
            }
        }
        // 关闭 fd（`N>&-`）
        for fd in &cmd.close_fds {
            let fd = *fd as i32;
            let saved = libc::dup(fd);
            if saved >= 0 && libc::close(fd) == 0 {
                guard.saved_closes.push((saved, fd));
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
    // `$(<file)`：读取文件内容（POSIX 特例）
    if tokens.len() == 2
        && matches!(tokens[0], Token::RedirIn)
        && let Token::Word(f) = &tokens[1]
    {
        return std::fs::read_to_string(f).ok();
    }
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
        output.push_str(&out);
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
        command.envs(cmd.env.iter().cloned());

        let mut dups = cmd.dup_fds.clone();
        if cmd.stderr_to_pipe {
            dups.push((2, 1));
        }
        let closes = cmd.close_fds.clone();
        let mut fd_files: Vec<std::fs::File> = Vec::new();
        let mut fd_dups: Vec<(i32, i32)> = Vec::new();
        for r in &cmd.fd_redirects {
            let file = if r.input {
                std::fs::File::open(&r.path).ok()
            } else {
                open_redirect(&r.path, r.append, cmd.force)
            };
            match file {
                Some(f) => {
                    let raw = f.as_raw_fd();
                    unsafe {
                        libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC);
                    }
                    fd_dups.push((raw, r.fd as i32));
                    fd_files.push(f);
                }
                None => {
                    eprintln!("shell: {}: cannot open", r.path);
                    cleanup_spawned_children(&mut children);
                    return None;
                }
            }
        }
        if !dups.is_empty() || !closes.is_empty() || !fd_dups.is_empty() {
            unsafe {
                command.pre_exec(move || {
                    for (from, to) in &dups {
                        libc::dup2(*to as i32, *from as i32);
                    }
                    for (src, dst) in &fd_dups {
                        libc::dup2(*src, *dst);
                    }
                    for fd in &closes {
                        libc::close(*fd as i32);
                    }
                    Ok(())
                });
            }
        }
        let _ = &fd_files;
        if let Some(ref hs) = cmd.here_string {
            let seq = HERESTR_SEQ.fetch_add(1, Ordering::SeqCst);
            let path = format!("/tmp/rbox_herestr_{}_{}", std::process::id(), seq);
            if std::fs::write(&path, format!("{}\n", hs)).is_ok()
                && let Ok(file) = std::fs::File::open(&path)
            {
                command.stdin(Stdio::from(file));
            }
        }
        if let Some(ref f) = cmd.rw_file
            && let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(f)
        {
            command.stdin(Stdio::from(file));
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
            Some(f) => match open_redirect(f, cmd.append, cmd.force) {
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
            match open_redirect(f, cmd.append_err, cmd.force) {
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

/// 拆分命令前导赋值：`VAR=val cmd` -> env + argv；纯赋值时 argv 为空。
/// 值会先做变量展开（与 bash 一致）。
fn split_assignments(cmds: &mut [SimpleCmd], last_rc: i32) -> bool {
    let mut ok = true;
    for cmd in cmds.iter_mut() {
        let mut assigns: Vec<(String, String)> = Vec::new();
        while let Some(first) = cmd.argv.first() {
            let Some((k, v)) = first.split_once('=') else {
                break;
            };
            let valid = !k.is_empty()
                && (k
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_'))
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid {
                break;
            }
            if super::builtin::is_readonly(k) {
                eprintln!("shell: {}: is read only", k);
                ok = false;
                break;
            }
            let val = crate::applets::core::shell::expander::expand_vars_clean(v, last_rc);
            assigns.push((k.to_string(), val));
            cmd.argv.remove(0);
        }
        if !assigns.is_empty() {
            cmd.env = assigns;
        }
    }
    ok
}

/// 解析复合命令尾部的重定向（如 `done < file`、`} > out`）为 SimpleCmd（argv 为空）。
/// 非重定向内容返回 None。
pub(crate) fn parse_redirect_tail(tail: &str) -> Option<SimpleCmd> {
    let tail = tail.trim();
    if tail.is_empty() {
        return None;
    }
    // 前置哑命令，使纯重定向序列也能被解析
    let tokens = tokenize(&format!("__rbox_dummy__ {}", tail));
    let cmd_list = build_command_list(&tokens).ok()?;
    let mut cmd: SimpleCmd = (*cmd_list.segments.first()?.pipeline.cmds.first()?).clone();
    if cmd.argv.len() != 1 || cmd.argv[0] != "__rbox_dummy__" {
        return None;
    }
    cmd.argv.clear();
    Some(cmd)
}

/// 展开反引号命令替换：`` `cmd` `` -> 输出（单引号内不展开）。
pub fn expand_backticks(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_squote = false;
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
        match c {
            b'\'' => {
                in_squote = true;
                out.push('\'');
                i += 1;
            }
            b'\\' => {
                out.push('\\');
                i += 1;
                if i < bytes.len() {
                    let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            b'`' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'`' {
                        break;
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    out.push('`');
                    i += 1;
                    continue;
                }
                let inner = &line[start..j];
                let inner = inner
                    .replace("\\`", "`")
                    .replace("\\\\", "\\")
                    .replace("\\$", "$");
                let inner = expand_backticks(&inner);
                let output = capture_output(&inner).unwrap_or_default();
                out.push_str(output.trim_end_matches('\n'));
                i = j + 1;
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

        let (program, extra_args) = if is_builtin(&cmd.argv[0]) {
            // 管道各段在子 shell 中执行：内置命令经 `rbox --builtin` 分发
            let rbox_path = std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/bin/rbox".to_string());
            (
                rbox_path,
                vec!["--builtin".to_string(), cmd.argv[0].clone()],
            )
        } else {
            resolve_command(&cmd.argv[0])
        };

        let mut command = Command::new(program);
        command.args(&extra_args);
        command.args(&cmd.argv[1..]);
        // 命令级环境变量（VAR=val cmd）
        command.envs(cmd.env.iter().cloned());

        // 所有子进程放入独立进程组（前后台统一）：
        // - 前台：便于 Ctrl-C/Ctrl-Z 按组转发信号；
        // - 后台：作业控制需要独立 pgid（fg/bg 按组 SIGCONT）。
        // `N>&M` 描述符复制在 stdio 设置完成后（pre_exec）应用，
        // 保证 `> file 2>&1` 中 stderr 指向已打开的文件。
        let mut dups = cmd.dup_fds.clone();
        if cmd.stderr_to_pipe {
            dups.push((2, 1));
        }
        let closes = cmd.close_fds.clone();
        // 任意 fd 重定向：父进程打开文件，pre_exec 中 dup2
        let mut fd_files: Vec<std::fs::File> = Vec::new();
        let mut fd_dups: Vec<(i32, i32)> = Vec::new();
        for r in &cmd.fd_redirects {
            let file = if r.input {
                std::fs::File::open(&r.path).ok()
            } else {
                open_redirect(&r.path, r.append, cmd.force)
            };
            match file {
                Some(f) => {
                    let raw = f.as_raw_fd();
                    unsafe {
                        libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC);
                    }
                    fd_dups.push((raw, r.fd as i32));
                    fd_files.push(f);
                }
                None => {
                    eprintln!("shell: {}: cannot open", r.path);
                    return 1;
                }
            }
        }
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
                for (src, dst) in &fd_dups {
                    libc::dup2(*src, *dst);
                }
                for fd in &closes {
                    libc::close(*fd as i32);
                }
                Ok(())
            });
        }
        let _ = &fd_files; // 保持文件打开至 spawn 之后

        // here-string：写入临时文件作为 stdin
        if let Some(ref hs) = cmd.here_string {
            let seq = HERESTR_SEQ.fetch_add(1, Ordering::SeqCst);
            let path = format!("/tmp/rbox_herestr_{}_{}", std::process::id(), seq);
            if std::fs::write(&path, format!("{}\n", hs)).is_ok()
                && let Ok(file) = std::fs::File::open(&path)
            {
                command.stdin(Stdio::from(file));
            }
        }
        if let Some(ref f) = cmd.rw_file
            && let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(f)
        {
            command.stdin(Stdio::from(file));
        }

        // `<>`：以 O_RDWR 打开并仅复制到 stdin（POSIX 语义）
        if let Some(ref f) = cmd.rw_file
            && let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(f)
        {
            command.stdin(Stdio::from(file));
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
            match open_redirect(f, cmd.append, cmd.force) {
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
            match open_redirect(f, cmd.append_err, cmd.force) {
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
            params::set_last_bg(pgid);
            let id = jobs::add_job(pgid, cmdline, jobs::JobState::Running);
            if id > 0 && super::options::monitor() {
                println!("[{}] {}", id, pgid);
            }
        }
        return 0;
    }

    // 设置前台进程组：用第一个子进程的 pid 作为 pgid，并把终端前台组交给它
    if let Some(first) = children.first() {
        let pgid = first.id() as i32;
        FOREGROUND_PGID.store(pgid, Ordering::Relaxed);
        tcsetpgrp_to(pgid);
    }

    // 前台等待期间：
    // 1. SIGCHLD 已在 spawn 前屏蔽（SigchldGuard），避免处理器用 waitpid(-1)
    //    抢收前台子进程导致 waitpid(pid) 返回 ECHILD。

    // 前台等待期间，启动一个线程监听 stdin 的 Ctrl-C（0x03 字节）。
    // raw 模式下 ISIG 已关闭，Ctrl-C 不产生信号，而是作为 0x03 字节到达。
    // shell 主线程在 wait() 中阻塞，无法读 stdin，所以需要单独线程。
    let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor_stop = stop_flag.clone();
    // 仅交互式 shell 且 stdin 为 tty 时监听 Ctrl-C 字节；
    // 子 shell/脚本不得读取 stdin（否则会吞掉父进程的待处理输入）；
    // 前台命令为嵌套交互式 shell（`sh`）时也不监听，否则会抢走子 shell 的输入
    let is_nested_shell = pipeline
        .cmds
        .first()
        .and_then(|c| c.argv.first())
        .map(|a| a == "sh")
        .unwrap_or(false);
    let monitor = if super::options::interactive()
        && !is_nested_shell
        && unsafe { libc::isatty(libc::STDIN_FILENO) } == 1
    {
        Some(std::thread::spawn(move || {
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
        }))
    } else {
        None
    };

    // 等待所有子进程，返回最后一个的退出码；检测到挂起则登记作业
    let mut last_code = 0;
    let mut pipefail_code: Option<i32> = None;
    for child in &mut children {
        let pid = child.id() as i32;
        let (code, stopped) = wait_child_pid(pid);
        last_code = code;
        if code != 0 {
            pipefail_code = Some(code);
        }
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
    if let Some(m) = monitor {
        let _ = m.join();
    }

    // 收割等待期间累积的后台僵尸（SIGCHLD 被屏蔽，处理器未执行）
    // 记录退出状态供 wait/jobs 使用
    jobs::reap_children();

    // SIGCHLD 由 SigchldGuard 在函数返回时恢复

    // 清除前台进程组标记，终端前台组归还 shell
    FOREGROUND_PGID.store(0, Ordering::Relaxed);
    tcsetpgrp_to(shell_pgid());

    // 如果是被 SIGINT 中断的，打印换行使提示符对齐
    if last_code == 130 {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\n");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    // pipefail：任一管道成员非零时返回最右侧非零状态
    if crate::applets::core::shell::options::pipefail()
        && let Some(code) = pipefail_code
    {
        last_code = code;
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

/// 注册 SIGCHLD 处理器：仅置位标志（async-signal-safe）；
/// 实际回收由 [`jobs::reap_children`] 在安全点执行并记录退出状态（供 wait/jobs 使用）。
pub fn install_sigchld_handler() {
    unsafe {
        libc::signal(libc::SIGCHLD, jobs::sigchld_handler as *const () as usize);
    }
}

/// 终端前台进程组：shell 自身 pgid。
fn shell_pgid() -> i32 {
    unsafe { libc::getpgrp() }
}

/// 把终端前台进程组设为 pgid（非 tty 或失败时忽略）。
fn tcsetpgrp_to(pgid: i32) {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        return;
    }
    unsafe {
        libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
    }
}

/// here-string 临时文件序号。
static HERESTR_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 命令是否存在（PATH 可执行文件或 rbox applet）：返回展示路径。
pub(crate) fn command_path(name: &str) -> Option<String> {
    if name.contains('/') {
        return is_executable(name).then(|| name.to_string());
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            let full = format!("{}/{}", dir, name);
            if is_executable(&full) {
                return Some(full);
            }
        }
    }
    // rbox 内置 applet
    if crate::applet::APPLETS.iter().any(|a| a.name() == name) {
        return Some(format!("rbox applet: {}", name));
    }
    None
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
pub(crate) fn resolve_command(cmd: &str) -> (String, Vec<String>) {
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
        let f = open_redirect(path, false, false);
        assert!(f.is_some());
        assert!(std::path::Path::new(path).exists());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_redirect_append_mode() {
        let path = "/tmp/rbox_test_redirect_append";
        let _ = std::fs::remove_file(path);
        // First write
        let _ = open_redirect(path, false, false);
        // Append should also work
        let f = open_redirect(path, true, false);
        assert!(f.is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_redirect_fails_for_invalid_path() {
        // Directory as target -> error
        let f = open_redirect("/tmp", false, false);
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
        assert!(apply_redirects(&cmd).unwrap().is_none());
    }

    #[test]
    fn builtin_redirects_error_for_unopenable_file() {
        // 目标为目录：打开失败，应返回错误码且不修改标准流
        let cmd = SimpleCmd {
            argv: vec!["pwd".into()],
            stdout_file: Some("/tmp".into()),
            ..Default::default()
        };
        assert_eq!(apply_redirects(&cmd).err(), Some(1));
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
