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
        "usage: shutdown\nSend SIGTERM to PID 1 to trigger orderly shutdown"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let rc = unsafe { libc::kill(1, libc::SIGTERM) };
        if rc == 0 {
            ExitCode::SUCCESS
        } else {
            eprintln!("shutdown: failed to signal init");
            ExitCode::from(1)
        }
    }
}
