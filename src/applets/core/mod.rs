//! 系统核心 applet：init 与配套管理工具。
pub mod init;
pub mod shell;
pub mod shutdown;
pub mod reboot;
pub mod status;
pub mod rservice;
pub(crate) mod control;

use std::fs;
use std::io::Write;

/// 输出日志：优先写入内核环形缓冲（/dev/kmsg，内核自动回显到 console，
/// 带时间戳且不重复）；kmsg 不可用时（如 devtmpfs 挂载前）回退 console stderr。
/// 注意：kmsg 每次 write 调用产生一条消息，必须整条一次性写入，
/// 否则会被拆成多行（每行带时间戳前缀）。
pub(crate) fn log(msg: &str) {
    if let Ok(mut kmsg) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let line = format!("rbox: {}\n", msg);
        let _ = kmsg.write_all(line.as_bytes());
        return;
    }
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{}", msg);
    let _ = stderr.flush();
}
