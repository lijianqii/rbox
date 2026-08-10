//! `date` - 显示/设置日期时间。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Date;
pub static DATE: &Date = &Date;

impl Applet for Date {
    fn name(&self) -> &'static str {
        "date"
    }
    fn help(&self) -> &'static str {
        "date - print current date and time"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let secs = unsafe { libc::time(std::ptr::null_mut()) };
        if secs < 0 {
            eprintln!("date: time() failed");
            return ExitCode::from(1);
        }
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&secs, &mut tm) };
        // 格式: Thu Aug  1 12:00:00 UTC 2024
        let days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let day = days.get(tm.tm_wday as usize).unwrap_or(&"???");
        let mon = months.get(tm.tm_mon as usize).unwrap_or(&"???");
        println!(
            "{} {} {:>2} {:02}:{:02}:{:02} UTC {}",
            day,
            mon,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            tm.tm_year + 1900
        );
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(DATE.name(), "date");
    }

    #[test]
    fn date_succeeds() {
        let _ = DATE.run(&[]);
    }
}
