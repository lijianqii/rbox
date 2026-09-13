//! `timeout` - 限时运行命令。
//!
//! 用法：timeout [-s SIGNAL] DURATION CMD [ARGS...]
//! DURATION 支持后缀 `s`/`m`/`h`（默认秒）与小数；超时退出码 124。

use crate::applet::Applet;
use std::process::{Command, ExitCode};

pub struct Timeout;
pub static TIMEOUT: &Timeout = &Timeout;

/// 解析时长（秒）。
pub(crate) fn parse_duration(s: &str) -> Option<f64> {
    let (num, mult) = match s.chars().last()? {
        's' | 'S' => (&s[..s.len() - 1], 1.0),
        'm' | 'M' => (&s[..s.len() - 1], 60.0),
        'h' | 'H' => (&s[..s.len() - 1], 3600.0),
        _ => (s, 1.0),
    };
    let v = num.parse::<f64>().ok()?;
    if v < 0.0 || !v.is_finite() {
        return None;
    }
    Some(v * mult)
}

/// 解析信号名/编号（复用 kill 的映射）。
fn parse_signal(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return (0..=64).contains(&n).then_some(n);
    }
    crate::applets::sys::kill::signal_number(s)
}

impl Applet for Timeout {
    fn name(&self) -> &'static str {
        "timeout"
    }
    fn help(&self) -> &'static str {
        "timeout [-s SIGNAL] DURATION CMD [ARGS...] - run command with time limit"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut signal = libc::SIGTERM;
        let mut i = 0;
        if args.first().map(String::as_str) == Some("-s") {
            i = 1;
            let Some(sig) = args.get(i) else {
                eprintln!("timeout: option -s requires an argument");
                return ExitCode::FAILURE;
            };
            match parse_signal(sig) {
                Some(s) => signal = s,
                None => {
                    eprintln!("timeout: invalid signal '{}'", sig);
                    return ExitCode::FAILURE;
                }
            }
            i += 1;
        }
        let Some(dur) = args.get(i) else {
            eprintln!("timeout: missing duration");
            return ExitCode::FAILURE;
        };
        let Some(secs) = parse_duration(dur) else {
            eprintln!("timeout: invalid duration '{}'", dur);
            return ExitCode::FAILURE;
        };
        i += 1;
        let Some(cmd) = args.get(i) else {
            eprintln!("timeout: missing command");
            return ExitCode::FAILURE;
        };
        let mut child = match Command::new(cmd).args(&args[i + 1..]).spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("timeout: {}: {}", cmd, e);
                return ExitCode::from(127);
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return ExitCode::from(status.code().unwrap_or(1) as u8);
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = unsafe { libc::kill(child.id() as i32, signal) };
                        // 给 2 秒宽限，之后 SIGKILL
                        let grace = std::time::Instant::now() + std::time::Duration::from_secs(2);
                        loop {
                            match child.try_wait() {
                                Ok(Some(_)) => return ExitCode::from(124),
                                Ok(None) if std::time::Instant::now() < grace => {
                                    std::thread::sleep(std::time::Duration::from_millis(50));
                                }
                                _ => {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    return ExitCode::from(124);
                                }
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => {
                    eprintln!("timeout: wait failed: {}", e);
                    return ExitCode::FAILURE;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(TIMEOUT.name(), "timeout");
        assert!(TIMEOUT.help().contains("time limit"));
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("5"), Some(5.0));
        assert_eq!(parse_duration("2m"), Some(120.0));
        assert_eq!(parse_duration("1h"), Some(3600.0));
        assert_eq!(parse_duration("0.5"), Some(0.5));
        assert_eq!(parse_duration("-1"), None);
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn timeout_kills_long_command() {
        // `timeout 0.2 sleep 5` 应快速返回 124
        let start = std::time::Instant::now();
        let rc = TIMEOUT.run(&["0.2".to_string(), "sleep".to_string(), "5".to_string()]);
        assert_eq!(rc, ExitCode::from(124));
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
    }

    #[test]
    fn passes_through_exit_code() {
        let rc = TIMEOUT.run(&["5".to_string(), "true".to_string()]);
        assert_eq!(rc, ExitCode::SUCCESS);
        let rc = TIMEOUT.run(&["5".to_string(), "false".to_string()]);
        assert_eq!(rc, ExitCode::from(1));
    }

    #[test]
    fn errors() {
        assert_eq!(TIMEOUT.run(&[]), ExitCode::FAILURE);
        assert_eq!(
            TIMEOUT.run(&["bad".to_string(), "true".to_string()]),
            ExitCode::FAILURE
        );
        assert_eq!(
            TIMEOUT.run(&[
                "-s".to_string(),
                "NOPE".to_string(),
                "1".to_string(),
                "true".to_string()
            ]),
            ExitCode::FAILURE
        );
    }
}
