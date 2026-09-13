//! `logkeeper` - 将内核日志（/dev/kmsg）持续转发到日志文件（日志持久化）。
//!
//! 用法：`logkeeper [FILE]`（缺省 /var/log/messages）
//!
//! 持久 rootfs（root=/dev/vda 磁盘模式）下日志落盘、重启不丢；
//! initramfs 内存模式下写内存（重启丢失，无害）。
//! 由服务单元管理（Restart=always，见 /etc/rbox/system/logkeeper.service.toml）。

use crate::applet::Applet;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;

pub struct Logkeeper;
pub static LOGKEEPER: &Logkeeper = &Logkeeper;

/// kmsg 单条消息可能超过 8KB（扩 buffer 重试上限）。
const MAX_BUF: usize = 1024 * 1024;

/// 日志文件轮转阈值（1MB）：超过则改名为 `<file>.1`（保留一份历史）。
const LOG_MAX_SIZE: u64 = 1024 * 1024;

/// 检查并轮转日志文件；返回是否需要重新打开。
pub(crate) fn rotate_if_needed(path: &str) -> std::io::Result<bool> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if meta.len() <= LOG_MAX_SIZE {
        return Ok(false);
    }
    let backup = format!("{}.1", path);
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(path, &backup)?;
    Ok(true)
}

/// 以 0600 打开日志文件（避免泄露服务输出）。
fn open_log(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

impl Applet for Logkeeper {
    fn name(&self) -> &'static str {
        "logkeeper"
    }
    fn help(&self) -> &'static str {
        "logkeeper [FILE] - forward /dev/kmsg to a log file (persistent logging)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let file = args
            .first()
            .map(String::as_str)
            .unwrap_or("/var/log/messages");
        // 确保日志文件父目录存在（initramfs 无 /var/log，磁盘模式自动创建）
        if let Some(parent) = std::path::Path::new(file).parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("logkeeper: cannot create {}: {}", parent.display(), e);
            return ExitCode::FAILURE;
        }
        let mut log = match open_log(file) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("logkeeper: cannot open {}: {}", file, e);
                return ExitCode::FAILURE;
            }
        };
        let mut kmsg = match std::fs::File::open("/dev/kmsg") {
            Ok(f) => f,
            Err(e) => {
                eprintln!("logkeeper: cannot open /dev/kmsg: {}", e);
                return ExitCode::FAILURE;
            }
        };
        // /dev/kmsg 无 EOF：非阻塞读，EAGAIN（暂时无消息）即休眠重试
        let fd = kmsg.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        }
        let mut buf = vec![0u8; 8192];
        let mut written: u64 = 0;
        loop {
            match std::io::Read::read(&mut kmsg, &mut buf) {
                Ok(0) => {
                    // kmsg 不会返回 0（无 EOF）；防御性处理
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Ok(n) => {
                    if log.write_all(&buf[..n]).is_err() {
                        eprintln!("logkeeper: write {} failed", file);
                        return ExitCode::FAILURE;
                    }
                    let _ = log.flush();
                    written += n as u64;
                    // 定期检查轮转（每次检查有 stat 开销，累计 256KB 才检查）
                    if written >= 256 * 1024 {
                        written = 0;
                        match rotate_if_needed(file) {
                            Ok(true) => match open_log(file) {
                                Ok(f) => log = f,
                                Err(e) => {
                                    eprintln!("logkeeper: reopen {} failed: {}", file, e);
                                    return ExitCode::FAILURE;
                                }
                            },
                            Ok(false) => {}
                            Err(e) => eprintln!("logkeeper: rotate {} failed: {}", file, e),
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // 暂时无新消息：休眠后重试
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                    // kmsg 要求单次 read buffer >= 消息长度：扩大重试
                    if buf.len() >= MAX_BUF {
                        eprintln!("logkeeper: kmsg message too large");
                        return ExitCode::FAILURE;
                    }
                    buf.resize(buf.len() * 2, 0);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    eprintln!("logkeeper: read /dev/kmsg failed: {}", e);
                    return ExitCode::FAILURE;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(LOGKEEPER.name(), "logkeeper");
        assert!(LOGKEEPER.help().contains("kmsg"));
    }

    #[test]
    fn rotate_small_file_not_needed() {
        let path = format!("/tmp/rbox_logkeeper_{}", std::process::id());
        std::fs::write(&path, b"small").unwrap();
        assert!(!rotate_if_needed(&path).unwrap());
        assert!(std::path::Path::new(&path).exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_large_file_moves_backup() {
        let path = format!("/tmp/rbox_logkeeper_big_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.1", path));
        std::fs::write(&path, vec![b'x'; (LOG_MAX_SIZE + 1) as usize]).unwrap();
        assert!(rotate_if_needed(&path).unwrap());
        assert!(!std::path::Path::new(&path).exists());
        assert!(std::path::Path::new(&format!("{}.1", path)).exists());
        let _ = std::fs::remove_file(format!("{}.1", path));
    }
}
