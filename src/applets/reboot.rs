//! `reboot` - 重启系统。
//!
//! 向 init (PID 1) 发送 SIGINT，触发有序关机后重启。
//! （init 的信号处理器收到 SIGINT 后执行 do_shutdown -> reboot(RB_AUTOBOOT)）
//! 当前简化实现：直接调用 reboot(RB_AUTOBOOT)。

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Reboot;
pub static REBOOT: &Reboot = &Reboot;

impl Applet for Reboot {
    fn name(&self) -> &'static str {
        "reboot"
    }
    fn help(&self) -> &'static str {
        "usage: reboot\nReboot the system"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        unsafe { libc::sync() };
        let rc = unsafe { libc::reboot(libc::RB_AUTOBOOT) };
        if rc == 0 {
            ExitCode::SUCCESS
        } else {
            eprintln!("reboot: failed");
            ExitCode::from(1)
        }
    }
}
