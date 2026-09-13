//! `uptime` - 显示运行时长与负载。
//!
//! 用法：uptime
//! 数据来源 /proc/uptime 与 /proc/loadavg。

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Uptime;
pub static UPTIME: &Uptime = &Uptime;

/// 把秒数格式化为 `D days, HH:MM` 或 `HH:MM`。
pub(crate) fn format_uptime(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let mins = (total % 3600) / 60;
    if days > 0 {
        format!(
            "{} day{}, {:2}:{:02}",
            days,
            if days == 1 { "" } else { "s" },
            hours,
            mins
        )
    } else {
        format!("{:2}:{:02}", hours, mins)
    }
}

/// 当前时钟 `HH:MM:SS`（UTC，与 date 命令一致）。
fn now_hms() -> String {
    let secs = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

impl Applet for Uptime {
    fn name(&self) -> &'static str {
        "uptime"
    }
    fn help(&self) -> &'static str {
        "uptime - show system uptime and load average"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| {
                s.split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
            })
            .unwrap_or(0.0);
        let loads = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let load: Vec<&str> = loads.split_whitespace().take(3).collect();
        let load_str = if load.len() == 3 {
            load.join(", ")
        } else {
            "0.00, 0.00, 0.00".to_string()
        };
        println!(
            " {} up {},  load average: {}",
            now_hms(),
            format_uptime(uptime),
            load_str
        );
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(UPTIME.name(), "uptime");
        assert!(UPTIME.help().contains("load"));
    }

    #[test]
    fn format_variants() {
        assert_eq!(format_uptime(0.0), " 0:00");
        assert_eq!(format_uptime(3661.0), " 1:01");
        assert_eq!(format_uptime(90061.0), "1 day,  1:01");
        assert_eq!(format_uptime(180122.0), "2 days,  2:02");
    }

    #[test]
    fn run_succeeds() {
        assert_eq!(UPTIME.run(&[]), ExitCode::SUCCESS);
    }
}
