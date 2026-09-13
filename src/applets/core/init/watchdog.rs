//! 硬件看门狗：打开设备、周期喂狗、poll 超时压缩。

use crate::applets::core::{LogLevel, log, log_at};

/// 打开硬件看门狗（打开即启动计数）。失败静默禁用（无设备环境不阻塞启动）。
pub(crate) fn open_watchdog(path: &str) -> Option<i32> {
    use std::ffi::CString;
    let p = CString::new(path).ok()?;
    let fd = unsafe { libc::open(p.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: watchdog unavailable ({}): watchdog disabled",
                path
            ),
        );
        None
    } else {
        log(&format!("rbox init: watchdog enabled on {}", path));
        Some(fd)
    }
}

/// 喂狗：写入任意字节。成功返回 true；失败返回 false（调用方禁用喂狗）。
pub(crate) fn feed_watchdog(fd: i32) -> bool {
    unsafe { libc::write(fd, b"V".as_ptr() as *const libc::c_void, 1) == 1 }
}

/// poll 超时与喂狗截止取 min：空闲时也能定时醒来喂狗。
/// `watchdog_active=false` 时原样返回（无喂狗约束）。
pub(crate) fn watchdog_poll_timeout(
    current_ms: i32,
    last_feed: &std::time::Instant,
    interval: &std::time::Duration,
    watchdog_active: bool,
) -> i32 {
    if !watchdog_active {
        return current_ms;
    }
    let since = last_feed.elapsed();
    let remain = if since >= *interval {
        0
    } else {
        (*interval - since).as_millis().min(i32::MAX as u128) as i32
    };
    if current_ms < 0 {
        remain // 原无限等待：改为按喂狗间隔唤醒
    } else {
        current_ms.min(remain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_poll_timeout_infinite_becomes_interval() {
        // 原无限等待（-1）：有喂狗约束时改为按喂狗间隔唤醒
        let last = std::time::Instant::now();
        let iv = std::time::Duration::from_secs(10);
        let t = watchdog_poll_timeout(-1, &last, &iv, true);
        assert!((9000..=10000).contains(&t), "t={t}");
    }

    #[test]
    fn watchdog_poll_timeout_min_with_existing() {
        // 已有更短超时（如 restart 退避 1s）保持；更长超时被喂狗截止压缩
        let last = std::time::Instant::now();
        let iv = std::time::Duration::from_secs(10);
        assert_eq!(watchdog_poll_timeout(500, &last, &iv, true), 500);
        let t = watchdog_poll_timeout(30_000, &last, &iv, true);
        assert!((9000..=10000).contains(&t), "t={t}");
    }

    #[test]
    fn watchdog_poll_timeout_inactive_passthrough() {
        // 未启用喂狗（无设备）：原超时原样返回，不影响事件驱动
        let last = std::time::Instant::now();
        let iv = std::time::Duration::from_secs(10);
        assert_eq!(watchdog_poll_timeout(-1, &last, &iv, false), -1);
        assert_eq!(watchdog_poll_timeout(200, &last, &iv, false), 200);
    }

    #[test]
    fn watchdog_poll_timeout_due_now() {
        // 已到喂狗时间：立即返回 0（poll 不等待，直接醒来喂狗）
        let last = std::time::Instant::now() - std::time::Duration::from_secs(10);
        let iv = std::time::Duration::from_secs(10);
        assert_eq!(watchdog_poll_timeout(-1, &last, &iv, true), 0);
    }
}
