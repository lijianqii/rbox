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
            | ":"
            | "readonly"
            | "getopts"
            | "ulimit"
            | "kill"
    )
}

/// 取下一个输入字节：优先消费前台命令监控线程缓存的 pending 队列，
/// 再读 fd 0（否则 `read` 会与监控线程抢输入）。
/// 从指定 fd 取下一个输入字节（`read -u FD`；默认 fd 0）。
fn next_input_byte_fd(fd: i32) -> Option<u8> {
    if fd == libc::STDIN_FILENO
        && let Ok(mut q) = super::executor::pending_stdin().lock()
        && let Some(b) = q.pop_front()
    {
        return Some(b);
    }
    loop {
        let mut b = [0u8; 1];
        let n = unsafe { libc::read(fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
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
    /// 输入 fd（`read -u FD`，默认 0）。
    fd: i32,
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
    let fd = opts.fd;
    let tty = unsafe { libc::isatty(fd) } == 1;
    if tty && let Some(p) = &opts.prompt {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), p.as_bytes());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    let echo = tty && !opts.silent;
    let mut bytes: Vec<u8> = Vec::new();
    let mut complete = false;
    while let Some(c) = next_input_byte_fd(fd) {
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

/// 只读变量集合（`readonly`）。
fn readonly_set() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// 是否为只读变量。
pub(crate) fn is_readonly(name: &str) -> bool {
    readonly_set()
        .lock()
        .map(|s| s.contains(name))
        .unwrap_or(false)
}

/// 标记只读。
fn mark_readonly(name: &str) {
    if let Ok(mut s) = readonly_set().lock() {
        s.insert(name.to_string());
    }
}

/// 文本规范化路径（逻辑路径：解析 `.` 与 `..`，不解析符号链接）。
pub(crate) fn logical_normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !path.starts_with('/') {
                    parts.push("..");
                }
            }
            c => parts.push(c),
        }
    }
    if path.starts_with('/') {
        format!("/{}", parts.join("/"))
    } else if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// 逻辑 PWD：$PWD 有效（与当前目录同一 inode）时返回，否则返回物理路径。
pub(crate) fn logical_pwd() -> String {
    if let Ok(p) = std::env::var("PWD")
        && !p.is_empty()
        && std::fs::metadata(&p).is_ok()
        && same_file_as_cwd(&p)
    {
        return p;
    }
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string())
}

/// $PWD 是否指向当前工作目录（同 dev+ino）。
fn same_file_as_cwd(path: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), std::fs::metadata(".")) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// 启动时初始化 PWD（未设置或失效时写入物理路径）。
pub(crate) fn init_pwd() {
    let valid = std::env::var("PWD")
        .ok()
        .filter(|p| !p.is_empty() && std::fs::metadata(p).is_ok() && same_file_as_cwd(p))
        .is_some();
    if !valid && let Ok(cwd) = std::env::current_dir() {
        setenv("PWD", cwd.to_string_lossy().as_ref());
    }
}

/// 未导出变量：`export -n` 后保留在 shell 内，不传给子进程。
static UNEXPORTED: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

/// 查询未导出变量值。
pub(crate) fn unexported_var(name: &str) -> Option<String> {
    UNEXPORTED
        .lock()
        .ok()
        .and_then(|m| m.as_ref().and_then(|map| map.get(name).cloned()))
}

/// 记录（Some）或移除（None）未导出变量。
pub(crate) fn set_unexported(name: &str, val: Option<String>) {
    if let Ok(mut m) = UNEXPORTED.lock() {
        let map = m.get_or_insert_with(std::collections::HashMap::new);
        match val {
            Some(v) => {
                map.insert(name.to_string(), v);
            }
            None => {
                map.remove(name);
            }
        }
    }
}

/// 设置环境变量（shell 单线程）。
fn setenv(k: &str, v: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: shell 单线程
    unsafe {
        std::env::set_var(k, v);
    }
}

/// 删除环境变量。
fn unsetenv(k: &str) {
    // SAFETY: shell 单线程
    unsafe {
        std::env::remove_var(k);
    }
}

/// getopts 解析位置（当前参数簇内下标；0 = 未开始）。
static GETOPTS_POS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
            // -P 物理路径（解析符号链接）；-L 逻辑路径（默认，保留 .. 文本语义）
            // `set -o physical` 时默认 -P
            let mut physical = crate::applets::core::shell::options::physical();
            let mut positional: Vec<&str> = Vec::new();
            for a in cmd.argv[1..].iter().map(String::as_str) {
                match a {
                    "-P" => physical = true,
                    "-L" => physical = false,
                    "-e" => {}
                    _ => positional.push(a),
                }
            }
            let target = match positional.first().copied() {
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
            // CDPATH：相对路径且非 . / .. 时依次尝试
            let mut candidates: Vec<String> = Vec::new();
            let relative =
                !target.starts_with('/') && !target.starts_with('.') && !target.starts_with('~');
            if relative && let Ok(cdpath) = std::env::var("CDPATH") {
                for dir in cdpath.split(':') {
                    if !dir.is_empty() {
                        candidates.push(format!("{}/{}", dir, target));
                    }
                }
            }
            candidates.push(target.clone());
            let old_pwd = logical_pwd();
            for cand in &candidates {
                if std::env::set_current_dir(cand).is_ok() {
                    if *cand != target {
                        println!("{}", cand);
                    }
                    let new_pwd = if physical {
                        std::env::current_dir()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| cand.clone())
                    } else {
                        let joined = if cand.starts_with('/') {
                            cand.clone()
                        } else {
                            format!("{}/{}", old_pwd.trim_end_matches('/'), cand)
                        };
                        logical_normalize(&joined)
                    };
                    setenv("PWD", &new_pwd);
                    setenv("OLDPWD", &old_pwd);
                    *last_rc = 0;
                    return BuiltinResult::Done;
                }
            }
            eprintln!("cd: {}: No such file or directory", target);
            *last_rc = 1;
            BuiltinResult::Done
        }
        "pwd" => {
            // -P 物理路径；默认 -L 逻辑路径（$PWD 有效时优先）
            let physical = cmd.argv[1..].iter().any(|a| a == "-P");
            if physical {
                match std::env::current_dir() {
                    Ok(p) => println!("{}", p.display()),
                    Err(e) => {
                        eprintln!("pwd: {}", e);
                        *last_rc = 1;
                    }
                }
            } else {
                println!("{}", logical_pwd());
            }
            BuiltinResult::Done
        }
        "export" => {
            let mut unexport = false;
            for arg in &cmd.argv[1..] {
                if arg == "-p" {
                    let mut vars: Vec<(String, String)> = std::env::vars().collect();
                    vars.sort();
                    for (k, v) in vars {
                        println!("export {}='{}'", k, v);
                    }
                    continue;
                }
                if arg == "-n" {
                    unexport = true;
                    continue;
                }
                if let Some(eq) = arg.find('=') {
                    let (k, v) = arg.split_at(eq);
                    let val = v[1..].to_string();
                    if unexport {
                        set_unexported(k, Some(val));
                        // SAFETY: single-threaded shell
                        unsafe {
                            std::env::remove_var(k);
                        }
                    } else {
                        set_unexported(k, None);
                        // SAFETY: single-threaded shell
                        unsafe {
                            std::env::set_var(k, &val);
                        }
                    }
                } else if unexport {
                    if let Ok(val) = std::env::var(arg) {
                        set_unexported(arg, Some(val));
                        // SAFETY: single-threaded shell
                        unsafe {
                            std::env::remove_var(arg);
                        }
                    }
                } else if let Some(val) = unexported_var(arg) {
                    // 重新 export：未导出变量移回环境
                    set_unexported(arg, None);
                    // SAFETY: single-threaded shell
                    unsafe {
                        std::env::set_var(arg, &val);
                    }
                }
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
                } else if is_readonly(arg) {
                    eprintln!("unset: {}: is read only", arg);
                } else {
                    set_unexported(arg, None);
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
            let args: Vec<&str> = cmd.argv[1..].iter().map(String::as_str).collect();
            if args.contains(&"-p") {
                for pid in jobs::pids() {
                    println!("{}", pid);
                }
            } else {
                let show_pid = args.contains(&"-l");
                for line in jobs::format_lines(show_pid) {
                    println!("{}", line);
                }
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
                    "-u" => {
                        i += 1;
                        match args.get(i).and_then(|v| v.parse::<i32>().ok()) {
                            Some(fd) => opts.fd = fd,
                            None => {
                                eprintln!("read: option -u requires an argument");
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
                            let on = a.starts_with('-');
                            match rest.get(i + 1).map(String::as_str) {
                                Some(name) => {
                                    i += 1;
                                    if !options::set_named(name, on) {
                                        eprintln!("set: unknown option: {}", name);
                                        rc = 2;
                                    }
                                }
                                None => {
                                    // 列出选项（+o 为可重置形式）
                                    for (name, value) in options::named_options() {
                                        if on {
                                            println!(
                                                "{:<15} {}",
                                                name,
                                                if value { "on" } else { "off" }
                                            );
                                        } else if value {
                                            println!("set -o {}", name);
                                        } else {
                                            println!("set +o {}", name);
                                        }
                                    }
                                }
                            }
                        }
                        _ if a.len() > 1 && (a.starts_with('-') || a.starts_with('+')) => {
                            let on = a.starts_with('-');
                            let sign = if on { "-" } else { "+" };
                            for c in a[1..].chars() {
                                match c {
                                    'a' => options::set_allexport(on),
                                    'b' => options::set_notify(on),
                                    'C' => options::set_noclobber(on),
                                    'e' => options::set_errexit(on),
                                    'f' => options::set_noglob(on),
                                    'm' => options::set_monitor(on),
                                    'n' => options::set_noexec(on),
                                    'u' => options::set_nounset(on),
                                    'v' => options::set_verbose(on),
                                    'x' => options::set_xtrace(on),
                                    other => {
                                        eprintln!("set: unknown option: {}{}", sign, other);
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
                    || !cmd.fd_redirects.is_empty()
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
            if specs.first().map(String::as_str) == Some("-n") {
                *last_rc = jobs::wait_any();
                return BuiltinResult::Done;
            }
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
        ":" => {
            *last_rc = 0;
            BuiltinResult::Done
        }
        "readonly" => {
            let mut rc = 0;
            let mut list = false;
            for arg in &cmd.argv[1..] {
                if arg == "-p" {
                    list = true;
                    continue;
                }
                if let Some((k, v)) = arg.split_once('=') {
                    mark_readonly(k);
                    // SAFETY: shell 单线程
                    unsafe {
                        std::env::set_var(k, v);
                    }
                } else {
                    mark_readonly(arg);
                }
            }
            if (list || cmd.argv.len() == 1)
                && let Ok(set) = readonly_set().lock()
            {
                let mut names: Vec<&String> = set.iter().collect();
                names.sort();
                for name in names {
                    let val = std::env::var(name).unwrap_or_default();
                    println!("readonly {}='{}'", name, val);
                }
            }
            *last_rc = rc;
            rc = 0;
            let _ = rc;
            BuiltinResult::Done
        }
        "getopts" => {
            *last_rc = run_getopts(cmd);
            BuiltinResult::Done
        }
        "ulimit" => {
            *last_rc = run_ulimit(cmd);
            BuiltinResult::Done
        }
        "kill" => {
            *last_rc = run_kill_builtin(cmd);
            BuiltinResult::Done
        }
        _ => BuiltinResult::NotBuiltin,
    }
}

/// `getopts optstring name [args...]`（POSIX）。返回 0 表示解析到选项，1 表示结束。
fn run_getopts(cmd: &SimpleCmd) -> i32 {
    let args: Vec<String> = cmd.argv[1..].to_vec();
    if args.len() < 2 {
        eprintln!("getopts: usage: getopts optstring name [arg...]");
        return 2;
    }
    let optstring = &args[0];
    let name = &args[1];
    let positional: Vec<String> = if args.len() > 2 {
        args[2..].to_vec()
    } else {
        params::all()
    };
    let silent = optstring.starts_with(':');
    let spec = optstring.trim_start_matches(':');
    let mut optind: usize = std::env::var("OPTIND")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut pos = GETOPTS_POS.load(std::sync::atomic::Ordering::SeqCst);
    let cluster: String = if pos == 0 {
        if optind > positional.len() {
            return 1;
        }
        let arg = positional[optind - 1].clone();
        if arg == "--" {
            setenv("OPTIND", (optind + 1).to_string());
            return 1;
        }
        if !arg.starts_with('-') || arg == "-" {
            return 1;
        }
        arg[1..].to_string()
    } else {
        // 继续上一簇：从环境恢复（存于 GETOPTS_CLUSTER 不可行，改用 OPTIND 指向当前参数）
        let arg = positional.get(optind - 1).cloned().unwrap_or_default();
        arg.trim_start_matches('-').to_string()
    };
    if cluster.is_empty() || pos >= cluster.len() {
        GETOPTS_POS.store(0, std::sync::atomic::Ordering::SeqCst);
        return 1;
    }
    let ch = cluster.as_bytes()[pos] as char;
    pos += 1;
    if pos >= cluster.len() {
        optind += 1;
        pos = 0;
    }
    GETOPTS_POS.store(pos, std::sync::atomic::Ordering::SeqCst);
    let mut optarg: Option<String> = None;
    let idx = spec.find(ch);
    match idx {
        Some(i) if spec.as_bytes().get(i + 1) == Some(&b':') => {
            // 需要参数
            let mut next_optind = optind;
            let rest = if pos > 0 {
                cluster[pos..].to_string()
            } else {
                String::new()
            };
            if !rest.is_empty() {
                optarg = Some(rest);
            } else {
                if next_optind > positional.len() {
                    if silent {
                        setenv(name, ":");
                        if let Some(ch) = Some(ch) {
                            setenv("OPTARG", ch.to_string());
                        }
                        setenv("OPTIND", next_optind.to_string());
                        return 0;
                    }
                    eprintln!("getopts: option requires an argument -- {}", ch);
                    setenv(name, "?");
                    setenv("OPTIND", next_optind.to_string());
                    return 0;
                }
                optarg = Some(positional[next_optind - 1].clone());
                next_optind += 1;
            }
            optind = next_optind;
            GETOPTS_POS.store(0, std::sync::atomic::Ordering::SeqCst);
        }
        Some(_) => {}
        None => {
            if silent {
                setenv(name, "?");
                setenv("OPTARG", ch.to_string());
                setenv("OPTIND", optind.to_string());
                return 0;
            }
            eprintln!("getopts: illegal option -- {}", ch);
            setenv(name, "?");
            setenv("OPTIND", optind.to_string());
            return 0;
        }
    }
    setenv(name, ch.to_string());
    match optarg {
        Some(v) => {
            setenv("OPTARG", v);
        }
        None => {
            unsetenv("OPTARG");
        }
    }
    setenv("OPTIND", optind.to_string());
    0
}

/// rlimit 资源类型（glibc 与 musl 签名不同）。
#[cfg(target_env = "gnu")]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type RlimitResource = libc::c_int;

/// `ulimit`：软/硬资源限制查看与设置。
fn run_ulimit(cmd: &SimpleCmd) -> i32 {
    fn fmt(v: u64) -> String {
        if v == libc::RLIM_INFINITY {
            "unlimited".to_string()
        } else {
            v.to_string()
        }
    }
    fn resource(opt: char) -> Option<RlimitResource> {
        Some(match opt {
            'c' => libc::RLIMIT_CORE,
            'd' => libc::RLIMIT_DATA,
            'f' => libc::RLIMIT_FSIZE,
            'n' => libc::RLIMIT_NOFILE,
            's' => libc::RLIMIT_STACK,
            't' => libc::RLIMIT_CPU,
            'v' => libc::RLIMIT_AS,
            'm' => libc::RLIMIT_RSS,
            'u' => libc::RLIMIT_NPROC,
            _ => return None,
        })
    }
    let args = &cmd.argv[1..];
    let mut hard = false;
    let mut soft = false;
    let mut all = false;
    let mut which: Option<char> = None;
    let mut value: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-H" {
            hard = true;
        } else if a == "-S" {
            soft = true;
        } else if a == "-a" {
            all = true;
        } else if let Some(opt) = a.strip_prefix('-') {
            if let Some(c) = opt.chars().next() {
                which = Some(c);
            }
        } else if let Ok(v) = a.parse::<u64>() {
            value = Some(v);
        } else if a == "unlimited" {
            value = Some(libc::RLIM_INFINITY);
        } else {
            eprintln!("ulimit: invalid argument: {}", a);
            return 1;
        }
        i += 1;
    }
    if !hard && !soft {
        soft = true;
    }
    let read = |r: RlimitResource, hard: bool| -> String {
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        unsafe { libc::getrlimit(r, &mut lim) };
        fmt(if hard { lim.rlim_max } else { lim.rlim_cur })
    };
    let list = |r: RlimitResource| {
        println!(
            "{:<16} (-{}) {}",
            "limit",
            match r {
                libc::RLIMIT_CORE => "c",
                libc::RLIMIT_DATA => "d",
                libc::RLIMIT_FSIZE => "f",
                libc::RLIMIT_NOFILE => "n",
                libc::RLIMIT_STACK => "s",
                libc::RLIMIT_CPU => "t",
                libc::RLIMIT_AS => "v",
                libc::RLIMIT_RSS => "m",
                libc::RLIMIT_NPROC => "u",
                _ => "?",
            },
            read(r, hard)
        );
    };
    if all {
        for r in [
            libc::RLIMIT_CORE,
            libc::RLIMIT_DATA,
            libc::RLIMIT_FSIZE,
            libc::RLIMIT_NOFILE,
            libc::RLIMIT_STACK,
            libc::RLIMIT_CPU,
            libc::RLIMIT_AS,
            libc::RLIMIT_NPROC,
        ] {
            list(r);
        }
        return 0;
    }
    let opt = which.unwrap_or('f');
    let Some(r) = resource(opt) else {
        eprintln!("ulimit: invalid option: -{}", opt);
        return 2;
    };
    match value {
        None => {
            println!("{}", read(r, hard));
            0
        }
        Some(v) => {
            let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
            unsafe { libc::getrlimit(r, &mut lim) };
            // 字节类限制按 KB 输入
            let scaled = match opt {
                'c' | 'd' | 'f' | 's' | 'v' | 'm' => {
                    if v == libc::RLIM_INFINITY {
                        v
                    } else {
                        v.saturating_mul(1024)
                    }
                }
                _ => v,
            };
            if soft || !hard {
                lim.rlim_cur = scaled;
            }
            if hard {
                lim.rlim_max = scaled;
            }
            if unsafe { libc::setrlimit(r, &lim) } != 0 {
                eprintln!("ulimit: {}", std::io::Error::last_os_error());
                return 1;
            }
            0
        }
    }
}

/// `kill` 内置：支持 `%job` 作业规格（其余与 kill applet 相同）。
fn run_kill_builtin(cmd: &SimpleCmd) -> i32 {
    use crate::applets::sys::kill::{parse_signal, signal_name, signal_number};
    let mut sig = libc::SIGTERM;
    let mut targets: Vec<String> = Vec::new();
    let mut i = 0;
    let args = &cmd.argv[1..];
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-l" || a == "--list" {
            // `kill -l [sig]`：数字→名称，名称→数字，无参→全部名称
            if let Some(arg) = args.get(i + 1) {
                if let Ok(n) = arg.parse::<i32>() {
                    if let Some(name) = signal_name(n) {
                        println!("{}", name);
                        return 0;
                    }
                } else if let Some(n) = signal_number(arg) {
                    println!("{}", n);
                    return 0;
                }
            }
            crate::applets::sys::kill::print_signal_table();
            return 0;
        }
        if a == "-s" || a == "--signal" {
            i += 1;
            let Some(name) = args.get(i) else {
                eprintln!("kill: option -s requires an argument");
                return 1;
            };
            match signal_number(name) {
                Some(n) => sig = n,
                None => {
                    eprintln!("kill: invalid signal '{}'", name);
                    return 1;
                }
            }
        } else if a.starts_with('-') && a.len() > 1 {
            match parse_signal(a) {
                Some(n) => sig = n,
                None => {
                    eprintln!("kill: invalid signal '{}'", a);
                    return 1;
                }
            }
        } else {
            targets.push(a.to_string());
        }
        i += 1;
    }
    if targets.is_empty() {
        eprintln!("kill: usage: kill [-SIGNAL] pid | %job ...");
        return 1;
    }
    let mut rc = 0;
    for t in targets {
        if let Some(spec) = t.strip_prefix('%') {
            let spec = format!("%{}", spec);
            match jobs::find(Some(&spec)) {
                Some(job) => {
                    if unsafe { libc::kill(-job.pgid, sig) } != 0 {
                        eprintln!("kill: {}: {}", t, std::io::Error::last_os_error());
                        rc = 1;
                    }
                }
                None => {
                    eprintln!("kill: {}: no such job", t);
                    rc = 1;
                }
            }
        } else if let Ok(pid) = t.parse::<i32>() {
            if unsafe { libc::kill(pid, sig) } != 0 {
                eprintln!("kill: {}: {}", pid, std::io::Error::last_os_error());
                rc = 1;
            }
        } else {
            eprintln!("kill: invalid pid: {}", t);
            rc = 1;
        }
    }
    rc
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

    // ─── ash 对齐：: / readonly / getopts / ulimit ───────────
    #[test]
    fn colon_is_builtin() {
        assert!(is_builtin(":"));
        let mut rc = 9;
        let r = try_builtin(&make_cmd(&[":", "ignored"]), &mut rc, &[]);
        assert!(matches!(r, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn readonly_protects_assignment() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["readonly", "RBOX_T_RO=1"]), &mut rc, &[]);
        assert!(is_readonly("RBOX_T_RO"));
        assert_eq!(std::env::var("RBOX_T_RO").unwrap(), "1");
    }

    #[test]
    fn getopts_parses_flags() {
        let mut rc = 0;
        try_builtin(
            &make_cmd(&["set", "--", "-a", "-b", "val", "x"]),
            &mut rc,
            &[],
        );
        try_builtin(&make_cmd(&["getopts", "ab:", "opt"]), &mut rc, &[]);
        assert_eq!(std::env::var("opt").unwrap(), "a");
        assert_eq!(std::env::var("OPTIND").unwrap(), "2");
        try_builtin(&make_cmd(&["getopts", "ab:", "opt"]), &mut rc, &[]);
        assert_eq!(std::env::var("opt").unwrap(), "b");
        assert_eq!(std::env::var("OPTARG").unwrap(), "val");
        // 非法选项 → '?'（非静默模式）
        try_builtin(&make_cmd(&["set", "--", "-z"]), &mut rc, &[]);
        setenv("OPTIND", "1");
        GETOPTS_POS.store(0, std::sync::atomic::Ordering::SeqCst);
        try_builtin(&make_cmd(&["getopts", "ab:", "opt"]), &mut rc, &[]);
        assert_eq!(std::env::var("opt").unwrap(), "?");
        // `--` 终止解析
        try_builtin(&make_cmd(&["set", "--", "--", "-a"]), &mut rc, &[]);
        setenv("OPTIND", "1");
        GETOPTS_POS.store(0, std::sync::atomic::Ordering::SeqCst);
        try_builtin(&make_cmd(&["getopts", "ab:", "opt"]), &mut rc, &[]);
        assert_eq!(rc, 1);
    }

    #[test]
    fn ulimit_shows_number() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["ulimit", "-n"]), &mut rc, &[]);
        assert_eq!(rc, 0);
    }

    #[test]
    fn readonly_unset_is_protected() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["readonly", "RBOX_T_RO2=5"]), &mut rc, &[]);
        assert!(is_readonly("RBOX_T_RO2"));
        try_builtin(&make_cmd(&["unset", "RBOX_T_RO2"]), &mut rc, &[]);
        assert!(is_readonly("RBOX_T_RO2"));
        assert_eq!(std::env::var("RBOX_T_RO2").unwrap(), "5");
    }

    #[test]
    fn ulimit_flags_succeed() {
        for args in [
            vec!["ulimit", "-n"],
            vec!["ulimit", "-H", "-n"],
            vec!["ulimit", "-S", "-n"],
            vec!["ulimit", "-a"],
            vec!["ulimit", "-c"],
        ] {
            let mut rc = 99;
            try_builtin(&make_cmd(&args), &mut rc, &[]);
            assert_eq!(rc, 0, "ulimit {:?} 应成功", args);
        }
    }

    #[test]
    fn kill_builtin_list_and_job_forms() {
        let mut rc = 99;
        try_builtin(&make_cmd(&["kill", "-l"]), &mut rc, &[]);
        assert_eq!(rc, 0);
        try_builtin(&make_cmd(&["kill", "-l", "9"]), &mut rc, &[]);
        assert_eq!(rc, 0);
        try_builtin(&make_cmd(&["kill", "-l", "KILL"]), &mut rc, &[]);
        assert_eq!(rc, 0);
        // 不存在的 %job 应报错返回非零
        let mut rc2 = 0;
        try_builtin(&make_cmd(&["kill", "%99"]), &mut rc2, &[]);
        assert_ne!(rc2, 0);
    }

    #[test]
    fn logical_normalize_cases() {
        assert_eq!(logical_normalize("/a/b/../c"), "/a/c");
        assert_eq!(logical_normalize("/a/./b//c/"), "/a/b/c");
        assert_eq!(logical_normalize("/.."), "/");
        assert_eq!(logical_normalize("a/../b"), "b");
        assert_eq!(logical_normalize("/"), "/");
    }

    #[test]
    fn cd_logical_and_physical_paths() {
        let _g = crate::applets::core::shell::compound::tests::test_guard();
        let base = format!("/tmp/rbox_cd_test_{}", std::process::id());
        let real = format!("{}/real", base);
        let sym = format!("{}/sym", base);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &sym).unwrap();
        let start = std::env::current_dir().unwrap();

        let mut rc = 0;
        // 默认 -L：保留符号链接路径；.. 按文本语义
        try_builtin(&make_cmd(&["cd", &sym]), &mut rc, &[]);
        assert_eq!(std::env::var("PWD").unwrap(), sym);
        try_builtin(&make_cmd(&["cd", ".."]), &mut rc, &[]);
        assert_eq!(std::env::var("PWD").unwrap(), base);
        // -P：物理路径
        try_builtin(&make_cmd(&["cd", "-P", &sym]), &mut rc, &[]);
        assert_eq!(std::env::var("PWD").unwrap(), real);
        // 回到起点，避免影响其他测试
        let _ = std::env::set_current_dir(&start);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn export_n_keeps_shell_var() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["export", "RBOX_T_EXP=7"]), &mut rc, &[]);
        assert_eq!(std::env::var("RBOX_T_EXP").unwrap(), "7");
        try_builtin(&make_cmd(&["export", "-n", "RBOX_T_EXP"]), &mut rc, &[]);
        assert!(std::env::var("RBOX_T_EXP").is_err());
        assert_eq!(unexported_var("RBOX_T_EXP").as_deref(), Some("7"));
        // 重新 export 回到环境
        try_builtin(&make_cmd(&["export", "RBOX_T_EXP"]), &mut rc, &[]);
        assert_eq!(std::env::var("RBOX_T_EXP").unwrap(), "7");
        assert!(unexported_var("RBOX_T_EXP").is_none());
        set_unexported("RBOX_T_EXP", None);
        unsafe {
            std::env::remove_var("RBOX_T_EXP");
        }
    }
}
