//! 内置命令：cd、exit、export、unset、pwd、history。

use super::types::SimpleCmd;
use super::{alias, functions, jobs, options, params, trap};

/// 内置命令执行结果。
pub enum BuiltinResult {
    /// `exit N`：退出 shell。
    Exit,
    /// 内置命令执行完成，继续下一行。
    Done,
    /// 不是内置命令。
    NotBuiltin,
}

/// 命令名是否为内置命令（`execute_line` 据此决定是否应用重定向，
/// 保证 `pwd > file` 等内置重定向与外部命令行为一致）。
pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "exit"
            | "cd"
            | "pwd"
            | "export"
            | "unset"
            | "history"
            | "alias"
            | "unalias"
            | "jobs"
            | "fg"
            | "bg"
            | "read"
            | "set"
            | "shift"
            | "exec"
            | "wait"
            | "disown"
            | "return"
            | "trap"
            | "type"
            | "hash"
            | "umask"
            | "let"
            | "times"
            | "local"
            | "break"
            | "continue"
    )
}

/// 取下一个输入字节：优先消费前台命令监控线程缓存的 pending 队列，
/// 再读 fd 0（否则 `read` 会与监控线程抢输入）。
fn next_input_byte() -> Option<u8> {
    if let Ok(mut q) = super::executor::pending_stdin().lock()
        && let Some(b) = q.pop_front()
    {
        return Some(b);
    }
    loop {
        let mut b = [0u8; 1];
        let n = unsafe { libc::read(libc::STDIN_FILENO, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n == 1 {
            return Some(b[0]);
        }
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return None;
    }
}

/// `read` 选项。
#[derive(Debug, Default, Clone)]
struct ReadOpts {
    raw: bool,
    silent: bool,
    timeout: Option<u64>,
    max_chars: Option<usize>,
    delim: u8,
    prompt: Option<String>,
}

/// 等待 stdin 可读（带超时）；返回 false 表示超时。
fn wait_input(timeout: Option<u64>) -> bool {
    let Some(secs) = timeout else {
        return true;
    };
    let mut fds = [libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    }];
    let n = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            1,
            (secs * 1000).min(i32::MAX as u64) as i32,
        )
    };
    n > 0
}

/// 从 stdin 读一行（带选项）。返回 (内容, 是否正常结束)。
fn read_line_from_stdin(opts: &ReadOpts) -> (String, bool) {
    let fd = libc::STDIN_FILENO;
    let tty = unsafe { libc::isatty(fd) } == 1;
    if let Some(p) = &opts.prompt {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), p.as_bytes());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    let echo = tty && !opts.silent;
    let mut bytes: Vec<u8> = Vec::new();
    let mut complete = false;
    while let Some(c) = next_input_byte() {
        if !wait_input(opts.timeout) {
            break; // 超时：返回已读内容
        }
        if c == opts.delim {
            complete = true;
            break;
        }
        if c == b'\n' || c == b'\r' {
            complete = true;
            if echo {
                let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\r\n");
            }
            break;
        }
        if c == 0x7f || c == 0x08 {
            if bytes.pop().is_some() && echo {
                let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x08 \x08");
            }
            continue;
        }
        bytes.push(c);
        if echo {
            let _ = std::io::Write::write_all(&mut std::io::stdout(), &[c]);
        }
        if let Some(max) = opts.max_chars
            && bytes.len() >= max
        {
            complete = true;
            break;
        }
    }
    if echo {
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if !opts.raw {
        line = unescape_read(&line);
    }
    (line, complete)
}

/// `read` 默认模式：反斜杠转义下一字符。
fn unescape_read(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 按 IFS 拆分并赋值；最后一个变量取剩余部分；无变量时写 REPLY。
fn assign_read_vars(line: &str, names: &[String]) {
    if names.is_empty() {
        unsafe {
            std::env::set_var("REPLY", line);
        }
        return;
    }
    let ifs = std::env::var("IFS").unwrap_or_else(|_| " \t\n".to_string());
    let mut rest = line;
    for (i, name) in names.iter().enumerate() {
        rest = rest.trim_start_matches(|c| ifs.contains(c));
        if i + 1 == names.len() {
            // 最后一个变量取剩余部分（去尾部 IFS，保留内部空白）
            let val = rest.trim_end_matches(|c| ifs.contains(c));
            // SAFETY: shell 单线程
            unsafe {
                std::env::set_var(name, val);
            }
            break;
        }
        let end = rest.find(|c| ifs.contains(c)).unwrap_or(rest.len());
        let (field, tail) = rest.split_at(end);
        // SAFETY: shell 单线程
        unsafe {
            std::env::set_var(name, field);
        }
        rest = tail;
    }
}

/// 尝试执行内置命令。
pub fn try_builtin(cmd: &SimpleCmd, last_rc: &mut i32, history: &[String]) -> BuiltinResult {
    if cmd.argv.is_empty() {
        return BuiltinResult::Done;
    }
    if !is_builtin(&cmd.argv[0]) {
        return BuiltinResult::NotBuiltin;
    }
    match cmd.argv[0].as_str() {
        "exit" => {
            // bash 语义：退出码取低 8 位（exit 300 -> 44）；非数字保持 last_rc
            let code = cmd
                .argv
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .map(|c| c & 0xff)
                .unwrap_or(*last_rc & 0xff);
            *last_rc = code;
            BuiltinResult::Exit
        }
        "cd" => {
            let target = match cmd.argv.get(1).map(String::as_str) {
                Some("-") => match std::env::var("OLDPWD") {
                    Ok(p) => {
                        println!("{}", p);
                        p
                    }
                    Err(_) => {
                        eprintln!("cd: OLDPWD not set");
                        *last_rc = 1;
                        return BuiltinResult::Done;
                    }
                },
                Some(p) => p.to_string(),
                None => std::env::var("HOME").unwrap_or_else(|_| "/".to_string()),
            };
            let old = std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            match std::env::set_current_dir(&target) {
                Ok(()) => {
                    if let Ok(new) = std::env::current_dir() {
                        // SAFETY: shell 单线程
                        unsafe {
                            std::env::set_var("PWD", new.to_string_lossy().as_ref());
                        }
                    }
                    if let Some(o) = old {
                        unsafe {
                            std::env::set_var("OLDPWD", o);
                        }
                    }
                    *last_rc = 0;
                }
                Err(e) => {
                    eprintln!("cd: {}: {}", target, e);
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "pwd" => {
            match std::env::current_dir() {
                Ok(p) => println!("{}", p.display()),
                Err(e) => {
                    eprintln!("pwd: {}", e);
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "export" => {
            for arg in &cmd.argv[1..] {
                if arg == "-p" {
                    let mut vars: Vec<(String, String)> = std::env::vars().collect();
                    vars.sort();
                    for (k, v) in vars {
                        println!("export {}={}", k, v);
                    }
                    continue;
                }
                if let Some(eq) = arg.find('=') {
                    let (k, v) = arg.split_at(eq);
                    // SAFETY: single-threaded shell
                    unsafe {
                        std::env::set_var(k, &v[1..]);
                    }
                }
                // `export VAR`（无 =）：变量已在环境中即为导出，no-op
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "unset" => {
            let mut func_mode = false;
            for arg in &cmd.argv[1..] {
                if arg == "-f" {
                    func_mode = true;
                    continue;
                }
                if arg == "-v" {
                    func_mode = false;
                    continue;
                }
                if func_mode {
                    functions::unset(arg);
                } else {
                    // SAFETY: single-threaded shell
                    unsafe {
                        std::env::remove_var(arg);
                    }
                }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "history" => {
            match cmd.argv.get(1).map(String::as_str) {
                Some("-c") => {
                    super::mark_history_clear();
                    *last_rc = 0;
                }
                Some(n) => match n.parse::<usize>() {
                    Ok(count) => {
                        let start = history.len().saturating_sub(count);
                        for (i, h) in history.iter().enumerate().skip(start) {
                            println!("  {}  {}", i + 1, h);
                        }
                        *last_rc = 0;
                    }
                    Err(_) => {
                        eprintln!("history: invalid count: {}", n);
                        *last_rc = 2;
                    }
                },
                None => {
                    for (i, h) in history.iter().enumerate() {
                        println!("  {}  {}", i + 1, h);
                    }
                    *last_rc = 0;
                }
            }
            BuiltinResult::Done
        }
        "alias" => {
            if cmd.argv.len() == 1 {
                for line in alias::list_aliases() {
                    println!("{}", line);
                }
                *last_rc = 0;
            } else {
                let mut rc = 0;
                for arg in &cmd.argv[1..] {
                    match arg.split_once('=') {
                        Some((name, value)) => {
                            if alias::set_alias(name, value).is_err() {
                                eprintln!("alias: invalid alias name: '{}'", name);
                                rc = 1;
                            }
                        }
                        None => match alias::get_alias(arg) {
                            Some(v) => println!("alias {}='{}'", arg, v),
                            None => {
                                eprintln!("alias: {}: not found", arg);
                                rc = 1;
                            }
                        },
                    }
                }
                *last_rc = rc;
            }
            BuiltinResult::Done
        }
        "unalias" => {
            for arg in &cmd.argv[1..] {
                alias::unalias(arg);
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "jobs" => {
            let show_pid = cmd.argv.iter().skip(1).any(|a| a == "-l");
            for line in jobs::format_lines(show_pid) {
                println!("{}", line);
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "fg" => {
            let spec = cmd.argv.get(1).map(String::as_str);
            match jobs::take(spec) {
                Some(job) => {
                    jobs::resume(job.pgid);
                    jobs::wait_pgid(job.pgid);
                    println!("[{}] done  {}", job.id, job.command);
                    *last_rc = 0;
                }
                None => {
                    eprintln!("fg: no current job");
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "bg" => {
            let spec = cmd.argv.get(1).map(String::as_str);
            match jobs::mark_running(spec) {
                Some(job) => {
                    jobs::resume(job.pgid);
                    println!("[{}]+ {} &", job.id, job.command);
                    *last_rc = 0;
                }
                None => {
                    eprintln!("bg: no current job");
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "read" => {
            let mut opts = ReadOpts {
                delim: b'\n',
                ..Default::default()
            };
            let mut names: Vec<String> = Vec::new();
            let args = &cmd.argv[1..];
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "-r" => opts.raw = true,
                    "-s" => opts.silent = true,
                    "-t" => {
                        i += 1;
                        match args.get(i).and_then(|v| v.parse::<u64>().ok()) {
                            Some(secs) => opts.timeout = Some(secs),
                            None => {
                                eprintln!("read: invalid timeout");
                                *last_rc = 2;
                                return BuiltinResult::Done;
                            }
                        }
                    }
                    "-n" => {
                        i += 1;
                        match args.get(i).and_then(|v| v.parse::<usize>().ok()) {
                            Some(n) => opts.max_chars = Some(n),
                            None => {
                                eprintln!("read: invalid count");
                                *last_rc = 2;
                                return BuiltinResult::Done;
                            }
                        }
                    }
                    "-d" => {
                        i += 1;
                        match args.get(i).and_then(|v| v.as_bytes().first().copied()) {
                            Some(c) => opts.delim = c,
                            None => {
                                eprintln!("read: option -d requires an argument");
                                *last_rc = 2;
                                return BuiltinResult::Done;
                            }
                        }
                    }
                    "-p" => {
                        i += 1;
                        match args.get(i) {
                            Some(p) => opts.prompt = Some(p.clone()),
                            None => {
                                eprintln!("read: option -p requires an argument");
                                *last_rc = 2;
                                return BuiltinResult::Done;
                            }
                        }
                    }
                    s if s.starts_with('-') && s.len() > 1 => {
                        eprintln!("read: unknown option: {}", s);
                        *last_rc = 2;
                        return BuiltinResult::Done;
                    }
                    s => names.push(s.to_string()),
                }
                i += 1;
            }
            let (line, complete) = read_line_from_stdin(&opts);
            if !complete && line.is_empty() {
                *last_rc = 1; // EOF / 超时
            } else {
                assign_read_vars(&line, &names);
                *last_rc = if complete { 0 } else { 1 };
            }
            BuiltinResult::Done
        }
        "set" => {
            let rest = &cmd.argv[1..];
            if rest.is_empty() {
                let mut vars: Vec<(String, String)> = std::env::vars().collect();
                vars.sort();
                for (k, v) in vars {
                    println!("{}={}", k, v);
                }
                *last_rc = 0;
            } else if rest[0] == "--" {
                params::set(rest[1..].to_vec());
                *last_rc = 0;
            } else {
                let mut rc = 0;
                let mut i = 0;
                while i < rest.len() {
                    let a = rest[i].as_str();
                    match a {
                        "-o" | "+o" => {
                            i += 1;
                            let on = a.starts_with('-');
                            match rest.get(i).map(String::as_str) {
                                Some("pipefail") => options::set_pipefail(on),
                                Some(other) => {
                                    eprintln!("set: unknown option: {}", other);
                                    rc = 2;
                                }
                                None => {
                                    eprintln!("set: -o requires an argument");
                                    rc = 2;
                                }
                            }
                        }
                        _ if a.len() > 1 && (a.starts_with('-') || a.starts_with('+')) => {
                            let on = a.starts_with('-');
                            for c in a[1..].chars() {
                                match c {
                                    'e' => options::set_errexit(on),
                                    'x' => options::set_xtrace(on),
                                    'u' => options::set_nounset(on),
                                    'C' => options::set_noclobber(on),
                                    other => {
                                        eprintln!(
                                            "set: unknown option: {}{}",
                                            if on { "-" } else { "+" },
                                            other
                                        );
                                        rc = 2;
                                    }
                                }
                            }
                        }
                        other => {
                            eprintln!("set: unknown option: {}", other);
                            rc = 2;
                        }
                    }
                    i += 1;
                }
                *last_rc = rc;
            }
            BuiltinResult::Done
        }
        "shift" => {
            let n = match cmd.argv.get(1) {
                Some(s) => match s.parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!("shift: invalid count '{}'", s);
                        *last_rc = 2;
                        return BuiltinResult::Done;
                    }
                },
                None => 1,
            };
            if params::shift(n) {
                *last_rc = 0;
            } else {
                eprintln!("shift: can't shift that many");
                *last_rc = 1;
            }
            BuiltinResult::Done
        }
        "exec" => {
            let args = &cmd.argv[1..];
            if args.is_empty() {
                // 仅重定向：请求 guard 不恢复（永久生效）
                if cmd.stdin_file.is_some()
                    || cmd.stdout_file.is_some()
                    || cmd.stderr_file.is_some()
                    || !cmd.dup_fds.is_empty()
                {
                    super::executor::persist_builtin_redirects();
                }
                *last_rc = 0;
                return BuiltinResult::Done;
            }
            let (program, extra) = super::executor::resolve_command(&args[0]);
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&program)
                .args(&extra)
                .args(&args[1..])
                .exec();
            eprintln!("exec: {}: {}", args[0], err);
            *last_rc = 127;
            options::request_exit(127);
            BuiltinResult::Done
        }
        "wait" => {
            jobs::reap_children();
            let specs: Vec<String> = cmd.argv[1..].to_vec();
            if specs.is_empty() {
                *last_rc = jobs::wait_all();
            } else {
                let mut rc = 0;
                for s in &specs {
                    if s.starts_with('%') {
                        match jobs::find(Some(s)) {
                            Some(job) => rc = jobs::wait_pgid(job.pgid),
                            None => {
                                eprintln!("wait: {}: no such job", s);
                                rc = 127;
                            }
                        }
                    } else if let Ok(pid) = s.parse::<i32>() {
                        rc = jobs::wait_pid(pid);
                    } else {
                        eprintln!("wait: {}: not a pid or job", s);
                        rc = 1;
                    }
                }
                *last_rc = rc;
            }
            BuiltinResult::Done
        }
        "disown" => {
            let args = &cmd.argv[1..];
            let mut rc = 0;
            if args.is_empty() {
                if !jobs::disown(None) {
                    rc = 1;
                }
            } else {
                for s in args {
                    if s == "-a" || s == "--all" {
                        while jobs::disown(None) {}
                    } else if !jobs::disown(Some(s)) {
                        eprintln!("disown: {}: no such job", s);
                        rc = 1;
                    }
                }
            }
            *last_rc = rc;
            BuiltinResult::Done
        }
        "return" => {
            let code = cmd
                .argv
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(*last_rc)
                & 0xff;
            *last_rc = code;
            options::request_return(code);
            BuiltinResult::Done
        }
        "trap" => {
            let args = &cmd.argv[1..];
            if args.is_empty() {
                for (sig, c) in trap::list() {
                    println!("trap -- '{}' {}", c, trap::signal_name(sig));
                }
                *last_rc = 0;
            } else if args[0] == "-l" {
                let names: Vec<String> = crate::applets::sys::kill::signal_names()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                println!("{}", names.join(" "));
                *last_rc = 0;
            } else {
                let cmdline = args[0].clone();
                let mut rc = 0;
                for sig_name in &args[1..] {
                    match trap::signal_number(sig_name) {
                        Some(n) => {
                            if cmdline == "-" {
                                trap::clear(n);
                            } else {
                                trap::set(n, &cmdline);
                                if n != 0 {
                                    trap::install_handlers();
                                }
                            }
                        }
                        None => {
                            eprintln!("trap: {}: invalid signal specification", sig_name);
                            rc = 1;
                        }
                    }
                }
                *last_rc = rc;
            }
            BuiltinResult::Done
        }
        "type" => {
            let mut rc = 0;
            for name in &cmd.argv[1..] {
                if let Some(v) = alias::get_alias(name) {
                    println!("{} is aliased to `{}'", name, v);
                } else if is_builtin(name) {
                    println!("{} is a shell builtin", name);
                } else if functions::is_function(name) {
                    println!("{} is a function", name);
                } else if let Some(p) = super::executor::command_path(name) {
                    println!("{} is {}", name, p);
                } else {
                    println!("{}: not found", name);
                    rc = 1;
                }
            }
            *last_rc = rc;
            BuiltinResult::Done
        }
        "hash" => {
            // 未实现命令哈希缓存：-r 清空为空操作
            *last_rc = 0;
            BuiltinResult::Done
        }
        "umask" => {
            let old = unsafe { libc::umask(0) };
            unsafe { libc::umask(old) };
            if let Some(m) = cmd.argv.get(1) {
                match u32::from_str_radix(m, 8) {
                    Ok(v) => {
                        unsafe { libc::umask(v) };
                        *last_rc = 0;
                    }
                    Err(_) => {
                        eprintln!("umask: invalid mode: {}", m);
                        *last_rc = 1;
                    }
                }
            } else {
                println!("{:04o}", old);
                *last_rc = 0;
            }
            BuiltinResult::Done
        }
        "let" => {
            let mut rc = 0;
            for expr in &cmd.argv[1..] {
                rc = if super::expander::eval_arith(expr).unwrap_or(0) != 0 {
                    0
                } else {
                    1
                };
            }
            *last_rc = rc;
            BuiltinResult::Done
        }
        "times" => {
            let mut t: libc::tms = unsafe { std::mem::zeroed() };
            unsafe { libc::times(&mut t) };
            let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
            let f = |v: libc::clock_t| format!("{:.3}", v as f64 / clk);
            println!("{} {}", f(t.tms_utime), f(t.tms_stime));
            println!("{} {}", f(t.tms_cutime), f(t.tms_cstime));
            *last_rc = 0;
            BuiltinResult::Done
        }
        "break" => {
            let n = cmd
                .argv
                .get(1)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            super::compound::request_break(n);
            *last_rc = 0;
            BuiltinResult::Done
        }
        "continue" => {
            let n = cmd
                .argv
                .get(1)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            super::compound::request_continue(n);
            *last_rc = 0;
            BuiltinResult::Done
        }
        "local" => {
            for arg in &cmd.argv[1..] {
                let (k, v) = match arg.split_once('=') {
                    Some((k, v)) => (k, Some(v)),
                    None => (arg.as_str(), None),
                };
                functions::push_local(k);
                if let Some(v) = v {
                    // SAFETY: shell 单线程
                    unsafe {
                        std::env::set_var(k, v);
                    }
                }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        _ => BuiltinResult::NotBuiltin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cwd 是进程全局状态，cd 测试串行化。
    fn cwd_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn make_cmd(args: &[&str]) -> SimpleCmd {
        SimpleCmd {
            argv: args.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn is_builtin_recognizes_all_builtins() {
        for name in [
            "exit", "cd", "pwd", "export", "unset", "history", "alias", "unalias", "jobs", "fg",
            "bg", "read", "set", "shift",
        ] {
            assert!(is_builtin(name), "{} 应为内置命令", name);
        }
        for name in ["echo", "ls", "true", "", "PWD"] {
            assert!(!is_builtin(name), "{} 不应为内置命令", name);
        }
    }

    #[test]
    fn unescape_read_strips_backslashes() {
        assert_eq!(unescape_read(r"a\ b"), "a b");
        assert_eq!(unescape_read("plain"), "plain");
        assert_eq!(unescape_read("trail\\"), "trail");
    }

    #[test]
    fn assign_read_vars_last_takes_rest() {
        let names = vec!["A".to_string(), "B".to_string()];
        assign_read_vars("one two three", &names);
        assert_eq!(std::env::var("A").unwrap(), "one");
        assert_eq!(std::env::var("B").unwrap(), "two three");
        let names = vec!["C".to_string()];
        assign_read_vars("  spaced  value  ", &names);
        assert_eq!(std::env::var("C").unwrap(), "spaced  value");
    }

    #[test]
    fn exit_returns_exit() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["exit"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Exit));
    }

    #[test]
    fn exit_with_code() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["exit", "42"]), &mut rc, &[]);
        assert_eq!(rc, 42);
    }

    #[test]
    fn exit_default_last_rc() {
        let mut rc = 7;
        try_builtin(&make_cmd(&["exit"]), &mut rc, &[]);
        assert_eq!(rc, 7);
    }

    #[test]
    fn cd_sets_cwd() {
        let _g = cwd_guard();
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["cd", "/tmp"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
        assert_eq!(std::env::current_dir().unwrap().to_string_lossy(), "/tmp");
    }

    #[test]
    fn cd_nonexistent_fails() {
        let _g = cwd_guard();
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["cd", "/nonexistent_xyz"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 1);
    }

    #[test]
    fn pwd_prints_cwd() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["pwd"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn export_sets_var() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["export", "RBOX_TEST=123"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(std::env::var("RBOX_TEST").unwrap(), "123");
        unsafe {
            std::env::remove_var("RBOX_TEST");
        }
    }

    #[test]
    fn unset_removes_var() {
        unsafe {
            std::env::set_var("RBOX_TEST_UNSET", "val");
        }
        let mut rc = 0;
        try_builtin(&make_cmd(&["unset", "RBOX_TEST_UNSET"]), &mut rc, &[]);
        assert!(std::env::var("RBOX_TEST_UNSET").is_err());
    }

    #[test]
    fn history_lists_entries() {
        let mut rc = 0;
        let history = vec!["echo a".to_string(), "echo b".to_string()];
        let result = try_builtin(&make_cmd(&["history"]), &mut rc, &history);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn unknown_returns_not_builtin() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["nonexistent_cmd"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::NotBuiltin));
    }

    #[test]
    fn empty_argv_returns_done() {
        let mut rc = 0;
        let result = try_builtin(&SimpleCmd::default(), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
    }

    // ─── cd 边界 ───────────────────────────────

    #[test]
    fn cd_to_root() {
        let _g = cwd_guard();
        let mut rc = 0;
        try_builtin(&make_cmd(&["cd", "/"]), &mut rc, &[]);
        assert_eq!(rc, 0);
    }

    #[test]
    fn cd_no_arg_goes_home() {
        let _g = cwd_guard();
        unsafe {
            std::env::set_var("HOME", "/tmp");
        }
        let mut rc = 0;
        try_builtin(&make_cmd(&["cd"]), &mut rc, &[]);
        assert_eq!(rc, 0);
        assert_eq!(std::env::current_dir().unwrap().to_string_lossy(), "/tmp");
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    // ─── export 边界 ───────────────────────────

    #[test]
    fn export_without_eq_sign() {
        // export VAR (no =) -> does nothing, just marks as exported
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["export", "RBOX_EMPTY"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn export_no_arg() {
        // export with no args -> Done (lists all vars, but we just check it doesn't fail)
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["export"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
    }

    // ─── unset 边界 ────────────────────────────

    #[test]
    fn unset_nonexistent_var() {
        // unset nonexistent -> no error
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["unset", "RBOX_NOEXIST"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    // ─── exit with invalid code ────────────────

    #[test]
    fn exit_with_non_numeric_code() {
        let mut rc = 5;
        try_builtin(&make_cmd(&["exit", "abc"]), &mut rc, &[]);
        // Non-numeric exit code -> keeps last rc
        assert_eq!(rc, 5);
    }

    #[test]
    fn exit_truncates_to_8_bits() {
        // bash 语义：exit 300 -> 300 & 0xff = 44
        let mut rc = 0;
        try_builtin(&make_cmd(&["exit", "300"]), &mut rc, &[]);
        assert_eq!(rc, 44);
        let mut rc = 0;
        try_builtin(&make_cmd(&["exit", "-1"]), &mut rc, &[]);
        assert_eq!(rc, 255);
    }
}
