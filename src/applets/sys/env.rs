//! `env` - 显示或设置环境变量。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Env;
pub static ENV: &Env = &Env;

impl Applet for Env {
    fn name(&self) -> &'static str { "env" }
    fn help(&self) -> &'static str { "env [VAR=val] [cmd] - show env or run cmd with modified env" }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut i = 0;
        // 解析 VAR=value 形式的参数
        while i < args.len() && args[i].contains('=') {
            let pair = args[i].splitn(2, '=').collect::<Vec<_>>();
            if pair.len() == 2 {
                // SAFETY: single-threaded applet context
                unsafe { std::env::set_var(pair[0], pair[1]); }
            }
            i += 1;
        }
        // 如果没有后续命令，打印所有环境变量
        if i >= args.len() {
            for (k, v) in std::env::vars() {
                println!("{}={}", k, v);
            }
            return ExitCode::SUCCESS;
        }
        // 执行后续命令
        let cmd = &args[i];
        let cmd_args = &args[i + 1..];
        match std::process::Command::new(cmd).args(cmd_args).status() {
            Ok(status) => {
                code_from_status(status)
            }
            Err(e) => {
                eprintln!("env: {}: {}", cmd, e);
                ExitCode::from(127)
            }
        }
    }
}

fn code_from_status(status: std::process::ExitStatus) -> ExitCode {
    if status.success() {
        ExitCode::SUCCESS
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(code) = status.code() {
                ExitCode::from(code as u8)
            } else {
                let signal = status.signal().unwrap_or(1);
                ExitCode::from(128 + (signal as u8))
            }
        }
        #[cfg(not(unix))]
        { ExitCode::from(1) }
    }
}
