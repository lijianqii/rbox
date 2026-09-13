//! PID 1 信号处理：self-pipe 唤醒、关机/重启标志、信号处理器安装。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// 全局关机标志：SIGTERM 信号处理器设置，主循环检查。
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 全局重启标志：SIGINT 信号处理器设置，主循环检查。
static REBOOT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// self-pipe 写端 fd：信号处理器写 1 字节唤醒主循环 poll；-1 表示未创建。
static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

/// 信号处理器：SIGTERM 设置关机标志，SIGINT 设置重启标志，SIGCHLD 仅唤醒；
/// 统一写 self-pipe 通知主循环（async-signal-safe：仅原子操作 + write）。
pub(crate) extern "C" fn signal_handler(sig: i32) {
    match sig {
        libc::SIGINT => REBOOT_REQUESTED.store(true, Ordering::SeqCst),
        libc::SIGTERM => SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst),
        _ => {} // SIGCHLD：仅唤醒主循环收割子进程
    }
    let fd = SIGNAL_PIPE_WRITE.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte: u8 = 1;
        unsafe { libc::write(fd, &byte as *const u8 as *const libc::c_void, 1) };
    }
}

/// 是否已请求关机或重启。
pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst) || REBOOT_REQUESTED.load(Ordering::SeqCst)
}

/// 安装 SIGTERM/SIGINT/SIGCHLD 信号处理器（sigaction + SA_RESTART）。
/// SIGCHLD 用于唤醒主循环收割子进程；SA_NOCLDSTOP 忽略子进程停止事件。
/// SIGHUP/SIGPIPE/SIGQUIT 显式忽略：PID 1 不能被这些信号终止
/// （tty 断开/写断管道/终端退格符都会触发，一旦命中即 kernel panic）。
pub(crate) fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        // 未设 SA_SIGINFO：内核按 sa_handler 形式调用单参数处理器。
        // sa_sigaction 与 sa_handler 为 union，这里直接以函数指针赋值。
        sa.sa_sigaction = signal_handler as extern "C" fn(i32) as usize;
        sa.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());
        // 以下三个信号保持运行，绝不终止 PID 1：
        // - SIGHUP：控制终端断开（串口拔出/会话首进程挂断）
        // - SIGPIPE：写已关闭的管道（日志/控制连接等）
        // - SIGQUIT：终端 \ 不应能 core dump PID 1
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }
}

/// 创建 self-pipe（两端 nonblocking + close-on-exec），返回 (读端, 写端)。
pub(crate) fn create_signal_pipe() -> (i32, i32) {
    let mut fds = [0i32; 2];
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return (-1, -1);
        }
        for fd in fds {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            let fdflags = libc::fcntl(fd, libc::F_GETFD);
            if fdflags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, fdflags | libc::FD_CLOEXEC);
            }
        }
    }
    (fds[0], fds[1])
}

/// 清空 self-pipe 读端（多次信号合并为一次，读空避免积压）。
pub(crate) fn drain_signal_pipe(fd: i32) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

/// 主循环注册 self-pipe 写端（信号处理器据此唤醒 poll）。
pub(crate) fn set_signal_pipe_write(fd: i32) {
    SIGNAL_PIPE_WRITE.store(fd, Ordering::SeqCst);
}

/// 是否已请求重启（SIGINT）。
pub(crate) fn reboot_requested() -> bool {
    REBOOT_REQUESTED.load(Ordering::SeqCst)
}
