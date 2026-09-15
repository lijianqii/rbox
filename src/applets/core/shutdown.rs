//! `shutdown` - 关机。
//!
//! 向 init (PID 1) 发送 SIGTERM，触发有序关机。

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Shutdown;
pub static SHUTDOWN: &Shutdown = &Shutdown;

impl Applet for Shutdown {
    fn name(&self) -> &'static str {
        "shutdown"
    }
    fn help(&self) -> &'static str {
        "usage: shutdown [-h|-r|-P] [-t SEC]\nSend SIGTERM to PID 1 to trigger orderly shutdown"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        // 支持 -h/--halt、-P/--poweroff（默认）、-r/--reboot、-t SEC（仅记录）、-c（取消：无操作）
        let mut reboot = false;
        for a in args {
            match a.as_str() {
                "-r" | "--reboot" => reboot = true,
                "-h" | "--halt" | "-P" | "--poweroff" => reboot = false,
                "-c" => {
                    eprintln!("shutdown: cancel not supported (no scheduled shutdown)");
                    return ExitCode::SUCCESS;
                }
                "-t" | "--time" => {
                    eprintln!(
                        "shutdown: -t is accepted but scheduled shutdown is not supported; acting immediately"
                    );
                }
                _ if a.starts_with("-t") => {}
                _ => {
                    eprintln!("shutdown: unknown option: {}", a);
                    return ExitCode::from(2);
                }
            }
        }
        if reboot {
            return crate::applets::core::reboot::REBOOT.run(&[]);
        }
        let rc = unsafe { libc::kill(1, libc::SIGTERM) };
        if rc == 0 {
            ExitCode::SUCCESS
        } else {
            eprintln!("shutdown: failed to signal init");
            ExitCode::from(1)
        }
    }
}

/// `poweroff`：与 `shutdown` 相同（SIGTERM 触发有序关机）。
pub struct Poweroff;
pub static POWEROFF: &Poweroff = &Poweroff;

impl Applet for Poweroff {
    fn name(&self) -> &'static str {
        "poweroff"
    }
    fn help(&self) -> &'static str {
        "usage: poweroff\nSend SIGTERM to PID 1 to trigger orderly poweroff"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        SHUTDOWN.run(&[])
    }
}

/// `halt`：停止系统（同 poweroff；内核随后断电）。
pub struct Halt;
pub static HALT: &Halt = &Halt;

impl Applet for Halt {
    fn name(&self) -> &'static str {
        "halt"
    }
    fn help(&self) -> &'static str {
        "usage: halt\nSend SIGTERM to PID 1 to trigger orderly halt"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        SHUTDOWN.run(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(SHUTDOWN.name(), "shutdown");
        assert!(SHUTDOWN.help().contains("SIGTERM"));
        assert_eq!(POWEROFF.name(), "poweroff");
        assert_eq!(HALT.name(), "halt");
    }
}
