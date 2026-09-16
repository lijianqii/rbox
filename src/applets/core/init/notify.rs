//! sd_notify 支持：Type=notify 服务的 READY=1 与 WATCHDOG=1 通知。
//!
//! init 在 /run/systemd/notify 上创建 Unix 数据报套接字（SO_PASSCRED），并把
//! `NOTIFY_SOCKET` 注入所有服务环境；主循环 poll 该套接字，按发送者 pid 匹配服务，
//! 处理 READY=1（服务就绪）、WATCHDOG=1（喂看门狗）、STATUS=（仅日志）。

use crate::applets::core::{LogLevel, log_at};
use std::os::unix::io::AsRawFd;

/// 通知套接字路径（与 systemd 一致）。
pub(crate) const NOTIFY_SOCKET: &str = "/run/systemd/notify";

/// 通知套接字句柄。
pub(crate) struct NotifySocket {
    sock: std::os::unix::net::UnixDatagram,
}

impl NotifySocket {
    pub(crate) fn fd(&self) -> i32 {
        self.sock.as_raw_fd()
    }

    /// 读取全部待处理通知；返回 (发送者 pid, 文本)。
    pub(crate) fn drain(&self) -> Vec<(i32, String)> {
        let mut out = Vec::new();
        loop {
            let mut buf = [0u8; 4096];
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: buf.len(),
            };
            let mut control = [0u8; 256];
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = control.len();
            let n = unsafe {
                libc::recvmsg(
                    self.sock.as_raw_fd(),
                    &mut msg,
                    libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
                )
            };
            if n <= 0 {
                break;
            }
            let mut pid: i32 = -1;
            unsafe {
                let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
                while !cmsg.is_null() {
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_CREDENTIALS
                    {
                        let cred = libc::CMSG_DATA(cmsg) as *const libc::ucred;
                        pid = (*cred).pid;
                    }
                    cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
                }
            }
            out.push((pid, String::from_utf8_lossy(&buf[..n as usize]).to_string()));
        }
        out
    }
}

/// 创建通知套接字（幂等）；失败返回 None（服务仍可运行，只是不等待 READY）。
pub(crate) fn create() -> Option<NotifySocket> {
    let _ = std::fs::create_dir_all("/run/systemd");
    let _ = std::fs::remove_file(NOTIFY_SOCKET);
    let sock = std::os::unix::net::UnixDatagram::bind(NOTIFY_SOCKET).ok()?;
    let enable: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            &enable as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    log_at(LogLevel::Debug, "rbox init: notify socket ready");
    Some(NotifySocket { sock })
}

/// 解析通知文本中的指令；返回 (ready, watchdog_ping, status, 服务主进程 pid)。
pub(crate) fn parse_message(text: &str) -> (bool, bool, Option<String>, Option<i32>) {
    let mut ready = false;
    let mut watchdog = false;
    let mut status = None;
    let mut ppid = None;
    for line in text.lines() {
        if line == "READY=1" {
            ready = true;
        } else if line == "WATCHDOG=1" {
            watchdog = true;
        } else if let Some(s) = line.strip_prefix("STATUS=") {
            status = Some(s.to_string());
        } else if let Some(s) = line.strip_prefix("X_RBOX_PPID=") {
            ppid = s.trim().parse::<i32>().ok();
        }
    }
    (ready, watchdog, status, ppid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_notify_messages() {
        assert_eq!(parse_message("READY=1"), (true, false, None, None));
        assert_eq!(parse_message("WATCHDOG=1"), (false, true, None, None));
        assert_eq!(
            parse_message("STATUS=serving\nREADY=1\nWATCHDOG=1"),
            (true, true, Some("serving".to_string()), None)
        );
        assert_eq!(parse_message("garbage"), (false, false, None, None));
        assert_eq!(
            parse_message("READY=1\nX_RBOX_PPID=42"),
            (true, false, None, Some(42))
        );
    }
}
