//! libc 系统调用封装（kill/sync/reboot）。

/// 发送信号给指定进程。
pub(crate) fn kill_process(pid: u32, sig: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(pid as i32, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 发送信号给进程组（pgid 由组首进程 pid 表示，kill 负 pid）。
pub(crate) fn kill_process_group(pgid: u32, sig: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(-(pgid as i32), sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 发送信号给所有进程（pid=-1）。
pub(crate) fn kill_all(sig: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(-1, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// sync 文件系统。
pub(crate) fn sync_fs() {
    unsafe { libc::sync() };
}

/// reboot 系统调用。
/// cmd: libc::RB_POWER_OFF（关机）或 libc::RB_AUTOBOOT（重启）
pub(crate) fn reboot_syscall(cmd: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::reboot(cmd) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
