//! `sleep` - 睡眠指定秒数。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Sleep;
pub static SLEEP: &Sleep = &Sleep;

/// 解析 sleep 参数，返回秒数或错误信息。
pub fn parse_sleep_arg(arg: &str) -> Result<f64, &'static str> {
    arg.parse::<f64>().map_err(|_| "invalid time interval")
}

impl Applet for Sleep {
    fn name(&self) -> &'static str {
        "sleep"
    }
    fn help(&self) -> &'static str {
        "sleep N - pause for N seconds"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("sleep: missing operand");
            return ExitCode::from(1);
        }
        match parse_sleep_arg(&args[0]) {
            Ok(secs) => {
                if secs > 0.0 {
                    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                }
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("sleep: {}: {}", msg, args[0]);
                ExitCode::from(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(SLEEP.name(), "sleep");
        assert!(SLEEP.help().contains("seconds"));
    }

    #[test]
    fn parse_integer() {
        assert_eq!(parse_sleep_arg("3"), Ok(3.0));
    }

    #[test]
    fn parse_fractional() {
        assert_eq!(parse_sleep_arg("0.5"), Ok(0.5));
    }

    #[test]
    fn parse_zero() {
        assert_eq!(parse_sleep_arg("0"), Ok(0.0));
    }

    #[test]
    fn parse_invalid() {
        assert!(parse_sleep_arg("notanumber").is_err());
    }

    #[test]
    fn parse_negative() {
        // Negative values parse but sleep does nothing (secs > 0.0 check)
        assert_eq!(parse_sleep_arg("-1"), Ok(-1.0));
    }
}
