//! `sleep` - 睡眠指定秒数。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Sleep;
pub static SLEEP: &Sleep = &Sleep;

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
        let secs: f64 = match args[0].parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("sleep: invalid time interval: {}", args[0]);
                return ExitCode::from(1);
            }
        };
        if secs > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
        }
        ExitCode::SUCCESS
    }
}
