//! 系统核心 applet：init 与配套管理工具。
pub(crate) mod control;
pub mod init;
pub mod reboot;
pub mod rgetty;
pub mod rlogin;
pub mod rservice;
pub mod shell;
pub mod shutdown;
pub mod status;

use std::fs;
use std::io::Write;

/// 日志级别。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    fn tag(self) -> &'static str {
        match self {
            LogLevel::Error => "E",
            LogLevel::Warn => "W",
            LogLevel::Info => "I",
            LogLevel::Debug => "D",
        }
    }
}

/// 当前日志级别（可通过 `RBOX_LOG` 环境变量调整，默认 Info）。
fn current_log_level() -> LogLevel {
    match std::env::var("RBOX_LOG").ok().as_deref() {
        Some("debug") | Some("DEBUG") | Some("d") => LogLevel::Debug,
        Some("warn") | Some("WARN") | Some("w") => LogLevel::Warn,
        Some("error") | Some("ERROR") | Some("e") => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

/// 输出带级别的日志：优先写入 /dev/kmsg，回退到 console stderr。
pub fn log_at(level: LogLevel, msg: &str) {
    if level > current_log_level() {
        return;
    }
    let tagged = format!("rbox {}: {}", level.tag(), msg);
    if let Ok(mut kmsg) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = kmsg.write_all(format!("{}\n", tagged).as_bytes());
        return;
    }
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{}", tagged);
    let _ = stderr.flush();
}

/// Info 级别日志（最常用，等价于原来的 `log()`）。
pub(crate) fn log(msg: &str) {
    log_at(LogLevel::Info, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_ordering() {
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
    }

    #[test]
    fn log_level_tags() {
        assert_eq!(LogLevel::Error.tag(), "E");
        assert_eq!(LogLevel::Warn.tag(), "W");
        assert_eq!(LogLevel::Info.tag(), "I");
        assert_eq!(LogLevel::Debug.tag(), "D");
    }
    #[test]
    fn current_log_level_parses_env() {
        // 环境变量全局共享，串行测试 + 收尾清理
        unsafe {
            std::env::set_var("RBOX_LOG", "debug");
            assert_eq!(current_log_level(), LogLevel::Debug);
            std::env::set_var("RBOX_LOG", "warn");
            assert_eq!(current_log_level(), LogLevel::Warn);
            std::env::set_var("RBOX_LOG", "ERROR");
            assert_eq!(current_log_level(), LogLevel::Error);
            std::env::set_var("RBOX_LOG", "garbage");
            assert_eq!(current_log_level(), LogLevel::Info); // 非法值回退默认
            std::env::remove_var("RBOX_LOG");
            assert_eq!(current_log_level(), LogLevel::Info); // 未设置回退默认
        }
    }

    #[test]
    fn log_at_filters_by_level() {
        // 仅验证过滤逻辑不 panic（输出到 stderr/kmsg，不断言内容）
        log_at(LogLevel::Debug, "debug msg");
        log_at(LogLevel::Error, "error msg");
    }
}
