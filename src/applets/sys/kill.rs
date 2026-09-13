//! `kill` - 向进程发送信号。
//!
//! 用法：kill [-SIGNAL] PID...
//!       kill -s SIGNAL PID...
//!       kill -l
//!
//! 默认发送 SIGTERM；信号可用名字（TERM/HUP/...，忽略 SIG 前缀）、数字。

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Kill;
pub static KILL: &Kill = &Kill;

/// 信号名（可带 SIG 前缀，大小写不敏感）-> 信号号。
pub(crate) fn signal_number(name: &str) -> Option<i32> {
    let upper = name
        .strip_prefix("SIG")
        .or_else(|| name.strip_prefix("sig"))
        .unwrap_or(name)
        .to_ascii_uppercase();
    let n = match upper.as_str() {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "ILL" => libc::SIGILL,
        "ABRT" => libc::SIGABRT,
        "FPE" => libc::SIGFPE,
        "KILL" => libc::SIGKILL,
        "SEGV" => libc::SIGSEGV,
        "PIPE" => libc::SIGPIPE,
        "ALRM" => libc::SIGALRM,
        "TERM" => libc::SIGTERM,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "CHLD" => libc::SIGCHLD,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        "TSTP" => libc::SIGTSTP,
        "TTIN" => libc::SIGTTIN,
        "TTOU" => libc::SIGTTOU,
        _ => return None,
    };
    Some(n)
}

/// 解析 `-SIGNAL` 形式的参数（`-9`、`-KILL`、`-s KILL` 中的 KILL 部分）。
pub(crate) fn parse_signal(arg: &str) -> Option<i32> {
    let rest = arg.strip_prefix('-')?;
    if rest.is_empty() {
        return None;
    }
    if let Ok(n) = rest.parse::<i32>() {
        return (0..=64).contains(&n).then_some(n);
    }
    signal_number(rest)
}

/// 支持的信号名列表（用于 `kill -l`）。
const SIGNAL_NAMES: &[&str] = &[
    "HUP", "INT", "QUIT", "ILL", "ABRT", "FPE", "KILL", "SEGV", "PIPE", "ALRM", "TERM", "USR1",
    "USR2", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU",
];

/// 信号号 -> 信号名（不带 SIG 前缀）。
pub(crate) fn signal_name(n: i32) -> Option<&'static str> {
    SIGNAL_NAMES
        .iter()
        .find(|name| signal_number(name) == Some(n))
        .copied()
}

impl Applet for Kill {
    fn name(&self) -> &'static str {
        "kill"
    }
    fn help(&self) -> &'static str {
        "kill [-SIGNAL] PID... | kill -l - send a signal to processes"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut sig = libc::SIGTERM;
        let mut pids: Vec<i32> = Vec::new();
        let mut list = false;
        let mut end_of_options = false;
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        i += 1;
                        continue;
                    }
                    "-l" | "--list" => {
                        i += 1;
                        // `kill -l [SIGNAL]`：带参数时打印映射并直接结束
                        if let Some(arg) = args.get(i) {
                            if let Ok(n) = arg.parse::<i32>() {
                                match signal_name(n) {
                                    Some(name) => println!("{}", name),
                                    None => {
                                        eprintln!("kill: invalid signal number '{}'", n);
                                        return ExitCode::FAILURE;
                                    }
                                }
                            } else {
                                match signal_number(arg) {
                                    Some(n) => println!("{}", n),
                                    None => {
                                        eprintln!("kill: invalid signal '{}'", arg);
                                        return ExitCode::FAILURE;
                                    }
                                }
                            }
                            return ExitCode::SUCCESS;
                        }
                        list = true;
                        continue;
                    }
                    "-s" | "--signal" => {
                        i += 1;
                        let Some(name) = args.get(i) else {
                            eprintln!("kill: option -s requires an argument");
                            return ExitCode::FAILURE;
                        };
                        match signal_number(name) {
                            Some(n) => sig = n,
                            None => {
                                eprintln!("kill: invalid signal '{}'", name);
                                return ExitCode::FAILURE;
                            }
                        }
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
                if a.starts_with('-') && a.len() > 1 {
                    match parse_signal(a) {
                        Some(n) => {
                            sig = n;
                            i += 1;
                            continue;
                        }
                        None => {
                            eprintln!("kill: invalid signal '{}'", a);
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            match a.parse::<i32>() {
                Ok(p) => pids.push(p),
                Err(_) => {
                    eprintln!("kill: invalid pid '{}'", a);
                    return ExitCode::FAILURE;
                }
            }
            i += 1;
        }

        if list {
            println!("{}", SIGNAL_NAMES.join(" "));
            return ExitCode::SUCCESS;
        }
        if pids.is_empty() {
            eprintln!("kill: usage: kill [-SIGNAL] PID...");
            return ExitCode::FAILURE;
        }

        let mut had_error = false;
        for pid in pids {
            if unsafe { libc::kill(pid, sig) } != 0 {
                eprintln!("kill: {}: {}", pid, std::io::Error::last_os_error());
                had_error = true;
            }
        }
        if had_error {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(KILL.name(), "kill");
        assert!(KILL.help().contains("signal"));
    }

    #[test]
    fn signal_names_with_and_without_prefix() {
        assert_eq!(signal_number("TERM"), Some(libc::SIGTERM));
        assert_eq!(signal_number("SIGTERM"), Some(libc::SIGTERM));
        assert_eq!(signal_number("sigkill"), Some(libc::SIGKILL));
        assert_eq!(signal_number("KILL"), Some(libc::SIGKILL));
        assert_eq!(signal_number("USR1"), Some(libc::SIGUSR1));
        assert_eq!(signal_number("NOPE"), None);
        assert_eq!(signal_number(""), None);
    }

    #[test]
    fn parse_signal_forms() {
        assert_eq!(parse_signal("-9"), Some(9));
        assert_eq!(parse_signal("-TERM"), Some(libc::SIGTERM));
        assert_eq!(parse_signal("-SIGTERM"), Some(libc::SIGTERM));
        assert_eq!(parse_signal("-0"), Some(0));
        assert_eq!(parse_signal("-999"), None); // 超出范围
        assert_eq!(parse_signal("-NOPE"), None);
        assert_eq!(parse_signal("9"), None); // 缺前导 -
    }

    #[test]
    fn kill_zero_signal_probes_process() {
        // 向自身发送 0 号信号：只做权限检查，应成功（绝不能发默认 SIGTERM）
        let pid = std::process::id().to_string();
        let rc = KILL.run(&["-0".to_string(), pid]);
        assert_eq!(rc, ExitCode::SUCCESS);
    }

    #[test]
    fn kill_invalid_pid_fails() {
        let rc = KILL.run(&["not-a-pid".to_string()]);
        assert_eq!(rc, ExitCode::FAILURE);
    }

    #[test]
    fn kill_missing_pid_fails() {
        assert_eq!(KILL.run(&[]), ExitCode::FAILURE);
    }

    #[test]
    fn kill_list_ok() {
        assert_eq!(KILL.run(&["-l".to_string()]), ExitCode::SUCCESS);
    }

    #[test]
    fn signal_name_reverse_mapping() {
        assert_eq!(signal_name(9), Some("KILL"));
        assert_eq!(signal_name(15), Some("TERM"));
        assert_eq!(signal_name(999), None);
    }

    #[test]
    fn kill_list_with_signal_arg() {
        assert_eq!(
            KILL.run(&["-l".to_string(), "9".to_string()]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            KILL.run(&["-l".to_string(), "TERM".to_string()]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            KILL.run(&["-l".to_string(), "NOPE".to_string()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn double_dash_allows_negative_pid() {
        // `kill -0 -- -1`：向 PID -1（所有进程）发 0 号信号（仅权限检查）
        let rc = KILL.run(&["-0".to_string(), "--".to_string(), "-1".to_string()]);
        assert_eq!(rc, ExitCode::SUCCESS);
    }
}
