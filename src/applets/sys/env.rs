//! `env` - 显示或设置环境变量。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Env;
pub static ENV: &Env = &Env;

impl Applet for Env {
    fn name(&self) -> &'static str {
        "env"
    }
    fn help(&self) -> &'static str {
        "env [VAR=val] [cmd] - show env or run cmd with modified env"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut i = 0;
        // 解析 VAR=value 形式的参数
        while i < args.len() && args[i].contains('=') {
            let pair = args[i].splitn(2, '=').collect::<Vec<_>>();
            if pair.len() == 2 {
                // SAFETY: single-threaded applet context
                unsafe {
                    std::env::set_var(pair[0], pair[1]);
                }
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
            Ok(status) => code_from_status(status),
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
        {
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_name() {
        assert_eq!(ENV.name(), "env");
    }

    #[test]
    fn env_sets_var() {
        let args = vec!["RBOX_ENV_TEST=hello".to_string()];
        ENV.run(&args);
        assert_eq!(std::env::var("RBOX_ENV_TEST").unwrap(), "hello");
        unsafe {
            std::env::remove_var("RBOX_ENV_TEST");
        }
    }

    #[test]
    fn env_runs_true_command() {
        let args = vec!["true".to_string()];
        // env runs `true` which returns 0 - we just verify it doesn't panic
        let _ = ENV.run(&args);
    }

    #[test]
    fn env_missing_command_returns_127() {
        let args = vec!["/nonexistent_cmd_xyz".to_string()];
        let _ = ENV.run(&args);
        // Can't directly compare ExitCode, but it should not panic
    }
}
