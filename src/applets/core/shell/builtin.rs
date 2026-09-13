//! 内置命令：cd、exit、export、unset、pwd、history。

use super::alias;
use super::jobs;
use super::params;
use super::types::SimpleCmd;

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

/// 从 stdin 读一行（raw 模式下逐字节读，tty 时回显 + 退格）。
/// 返回 (内容, 是否以换行结束)。
fn read_line_from_stdin(raw: bool) -> (String, bool) {
    let fd = libc::STDIN_FILENO;
    let tty = unsafe { libc::isatty(fd) } == 1;
    let mut bytes: Vec<u8> = Vec::new();
    let mut complete = false;
    while let Some(c) = next_input_byte() {
        match c {
            b'\n' => {
                complete = true;
                break;
            }
            b'\r' => {
                complete = true;
                if tty {
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\r\n");
                }
                break;
            }
            0x7f | 0x08 => {
                if bytes.pop().is_some() && tty {
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x08 \x08");
                }
            }
            c => {
                bytes.push(c);
                if tty {
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), &[c]);
                }
            }
        }
    }
    if tty {
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if !raw {
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

/// 按 IFS 空白拆分并赋值；最后一个变量取剩余部分；无变量时写 REPLY。
fn assign_read_vars(line: &str, names: &[String]) {
    if names.is_empty() {
        unsafe {
            std::env::set_var("REPLY", line);
        }
        return;
    }
    let mut rest = line.trim_start_matches([' ', '\t']);
    for (i, name) in names.iter().enumerate() {
        if i + 1 == names.len() {
            let val = rest.trim_end_matches([' ', '\t']);
            unsafe {
                std::env::set_var(name, val);
            }
            break;
        }
        let end = rest.find([' ', '\t']).unwrap_or(rest.len());
        let (field, tail) = rest.split_at(end);
        unsafe {
            std::env::set_var(name, field);
        }
        rest = tail.trim_start_matches([' ', '\t']);
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
            let target = cmd
                .argv
                .get(1)
                .cloned()
                .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));
            match std::env::set_current_dir(&target) {
                Ok(()) => *last_rc = 0,
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
                if let Some(eq) = arg.find('=') {
                    let (k, v) = arg.split_at(eq);
                    // SAFETY: single-threaded shell
                    unsafe {
                        std::env::set_var(k, &v[1..]);
                    }
                }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "unset" => {
            for arg in &cmd.argv[1..] {
                // SAFETY: single-threaded shell
                unsafe {
                    std::env::remove_var(arg);
                }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "history" => {
            for (i, h) in history.iter().enumerate() {
                println!("  {}  {}", i + 1, h);
            }
            *last_rc = 0;
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
            for line in jobs::format_lines() {
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
            let raw = cmd.argv.iter().skip(1).any(|a| a == "-r");
            let names: Vec<String> = cmd.argv[1..]
                .iter()
                .filter(|a| !a.starts_with('-'))
                .cloned()
                .collect();
            let (line, complete) = read_line_from_stdin(raw);
            if !complete && line.is_empty() {
                *last_rc = 1; // EOF
            } else {
                assign_read_vars(&line, &names);
                *last_rc = if complete { 0 } else { 1 };
            }
            BuiltinResult::Done
        }
        "set" => {
            let rest = &cmd.argv[1..];
            if rest.is_empty() {
                for (k, v) in std::env::vars() {
                    println!("{}={}", k, v);
                }
                *last_rc = 0;
            } else if rest[0] == "--" {
                params::set(rest[1..].to_vec());
                *last_rc = 0;
            } else {
                eprintln!("set: only 'set -- args...' is supported");
                *last_rc = 2;
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
        _ => BuiltinResult::NotBuiltin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["cd", "/tmp"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
        assert_eq!(std::env::current_dir().unwrap().to_string_lossy(), "/tmp");
    }

    #[test]
    fn cd_nonexistent_fails() {
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
        let mut rc = 0;
        try_builtin(&make_cmd(&["cd", "/"]), &mut rc, &[]);
        assert_eq!(rc, 0);
    }

    #[test]
    fn cd_no_arg_goes_home() {
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
