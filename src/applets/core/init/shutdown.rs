//! 有序关机/重启：逆序停止服务、清理残留进程、sync 后 power off/reboot。

use crate::applets::core::init::services::{ServiceInstance, stop_service_instance};
use crate::applets::core::init::signals::reboot_requested;
use crate::applets::core::init::syscall::{kill_all, reboot_syscall, sync_fs};
use crate::applets::core::{LogLevel, log, log_at};
use std::process::ExitCode;

/// 关机总超时（秒）：逐服务 stop + 残留进程回收共用此 deadline，
/// 到点后直接 SIGKILL 全部残留进程，避免被忽略 SIGTERM 的进程拖住。
const SHUTDOWN_TIMEOUT_SECS: u64 = 10;

/// 执行有序关机：逆序停止服务，杀残留进程，再 power off。
pub(crate) fn do_shutdown(services: &mut [ServiceInstance]) -> ExitCode {
    log("rbox init: shutting down");
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(SHUTDOWN_TIMEOUT_SECS);
    for svc in services.iter_mut().rev() {
        if std::time::Instant::now() >= deadline {
            log_at(
                LogLevel::Warn,
                "rbox init: shutdown deadline reached, skipping remaining services",
            );
            break;
        }
        stop_service_instance(svc);
    }
    log("rbox init: sending SIGTERM to all processes");
    let _ = kill_all(libc::SIGTERM);
    // 等待所有子进程退出（受总 deadline 约束）；到点升级 SIGKILL，避免忽略
    // SIGTERM 的进程无限拖延关机。
    loop {
        if std::time::Instant::now() >= deadline {
            log("rbox init: sending SIGKILL to all processes");
            let _ = kill_all(libc::SIGKILL);
            // 给 SIGKILL 一个极短收割窗口（最多 1 秒）
            let kill_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while std::time::Instant::now() < kill_deadline {
                let mut status: libc::c_int = 0;
                let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                if pid < 0 {
                    break; // ECHILD：无子进程
                }
                if pid == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            break;
        }
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid < 0 {
            break; // ECHILD：无子进程
        }
        if pid == 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // pid > 0：已收割一个，立即继续收割其余
    }
    sync_fs();
    // 有序关机：先把根文件系统 remount 只读，再触发 reboot 系统调用
    // （initramfs 根已是 tmpfs/rootfs 且不可 remount ro 时忽略失败，仅尽力而为）
    let root = std::ffi::CString::new("/").unwrap();
    let opts = std::ffi::CString::new("remount,ro").unwrap();
    let _ = unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT,
            opts.as_ptr() as *const libc::c_void,
        )
    };
    let is_reboot = reboot_requested();
    if is_reboot {
        log("rbox init: rebooting");
    } else {
        log("rbox init: power off");
    }
    let action = if is_reboot {
        libc::RB_AUTOBOOT
    } else {
        libc::RB_POWER_OFF
    };
    if let Err(e) = reboot_syscall(action) {
        log_at(
            LogLevel::Error,
            &format!("rbox init: reboot syscall failed: {}", e),
        );
        // 重启失败时回退到关机；仍失败则挂起等待人工干预
        if is_reboot {
            let _ = reboot_syscall(libc::RB_POWER_OFF);
        }
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
