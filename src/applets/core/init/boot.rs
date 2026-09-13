//! 启动模式（normal/single/emergency）与应急/单用户 shell。

use crate::applets::core::init::services::spawn_fresh_shell;
use crate::applets::core::init::shutdown::do_shutdown;
use crate::applets::core::init::signals::shutdown_requested;
use crate::applets::core::init::syscall::kill_all;
use crate::applets::core::init::watchdog::{feed_watchdog, open_watchdog};
use crate::applets::core::{LogLevel, log, log_at};
use std::process::ExitCode;

/// 单元加载/依赖解析失败时的降级路径：循环拉起一个 emergency shell。
/// 轮询等待 shell 退出并同时响应关机标志（SIGTERM 到来时不再等 shell 退出）。
/// 启动模式：内核 cmdline 的 `single`/`emergency` 单词决定。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BootMode {
    /// 正常启动（默认）
    Normal,
    /// 单用户模式：跳过服务，进 root shell
    Single,
    /// 应急模式：跳过服务，进 emergency shell
    Emergency,
}

/// 从 /proc/cmdline 解析启动模式：包含单词 `emergency` 或 `single` 时生效
/// （精确单词匹配，避免误匹配 root=/dev/single 之类参数）。
pub(crate) fn boot_mode_from_cmdline() -> BootMode {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    boot_mode_from_words(&words)
}

/// 从 cmdline 单词列表解析启动模式（纯函数，便于单测）。
pub(crate) fn boot_mode_from_words(words: &[&str]) -> BootMode {
    if words.contains(&"emergency") {
        BootMode::Emergency
    } else if words.contains(&"single") {
        BootMode::Single
    } else {
        BootMode::Normal
    }
}

/// 应急/单用户 shell：跳过单元加载，循环 spawn root shell；
/// shell 退出后重新拉起，期间响应关机标志（terminate shell 后进入关机流程）。
/// `reason` 用于日志区分（no units / emergency / single）。
pub(crate) fn run_emergency_shell(reason: &str) -> ExitCode {
    log_at(
        LogLevel::Error,
        &format!("rbox init: {} mode, emergency shell", reason),
    );
    // 应急/单用户模式也喂狗：避免诊断期间被硬件看门狗复位打断
    let cfg = crate::config::load();
    let watchdog_interval = std::time::Duration::from_secs(cfg.init.watchdog_interval);
    let mut watchdog_fd = if cfg.init.watchdog_interval > 0 {
        open_watchdog(&cfg.init.watchdog_path)
    } else {
        None
    };
    let mut last_feed = std::time::Instant::now();
    loop {
        if let Some(fd) = watchdog_fd
            && last_feed.elapsed() >= watchdog_interval
        {
            if feed_watchdog(fd) {
                last_feed = std::time::Instant::now();
            } else {
                log_at(
                    LogLevel::Warn,
                    "rbox init: watchdog write failed, disabling",
                );
                watchdog_fd = None;
            }
        }
        if shutdown_requested() {
            return do_shutdown(&mut []);
        }
        let mut child = match spawn_fresh_shell() {
            Some(c) => c,
            None => {
                log("rbox init: cannot spawn emergency shell, waiting");
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }
        };
        // 轮询等待 shell 退出；期间响应关机标志（终止 shell 后进入关机流程）
        loop {
            if shutdown_requested() {
                let _ = kill_all(libc::SIGTERM);
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_mode_from_words_matches_exact_words() {
        assert_eq!(boot_mode_from_words(&[]), BootMode::Normal);
        assert_eq!(
            boot_mode_from_words(&["console=ttyAMA0", "rdinit=/init"]),
            BootMode::Normal
        );
        assert_eq!(
            boot_mode_from_words(&["root=/dev/vda", "single"]),
            BootMode::Single
        );
        assert_eq!(boot_mode_from_words(&["emergency"]), BootMode::Emergency);
        // emergency 优先于 single（systemd 语义：emergency 更深）
        assert_eq!(
            boot_mode_from_words(&["single", "emergency"]),
            BootMode::Emergency
        );
        // 非精确单词不误匹配（root=/dev/single 之类）
        assert_eq!(
            boot_mode_from_words(&["root=/dev/single"]),
            BootMode::Normal
        );
        assert_eq!(
            boot_mode_from_words(&["console=ttyAMA0,emergency"]),
            BootMode::Normal
        );
    }
}
