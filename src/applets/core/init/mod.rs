//! `init` - PID 1 系统初始化进程。
//!
//! systemd 风格的 target 依赖链启动（配置使用 TOML 格式）：
//! - 解析 `/etc/rbox/system/` 下的 `.toml` 单元文件（units 模块）。
//! - 支持 `[Unit]` 的 `After=`/`Requires=`、`[Install]` 的 `WantedBy=`、
//!   `[Service]` 的 `Type=`(simple/forking)/`ExecStart=` 等（services 模块）。
//! - 从 `default.target` 出发，按依赖拓扑序启动服务。
//! - 启动完成后 fork 一个 shell（作为 getty 替代），init 作为 PID 1 常驻，
//!   回收僵尸/孤儿进程；shell 退出后重新 fork。
//! - 通过 unix socket 响应控制请求（server 模块：status/start/stop/restart/reload）。

pub(crate) mod mount;
pub(crate) mod server;
pub(crate) mod services;
pub(crate) mod syscall;
pub(crate) mod units;

use crate::applet::Applet;
use crate::applets::core::init::mount::{
    apply_sysctl, mount_all_fs, setup_environment, setup_hostname,
};
use crate::applets::core::init::server::{create_status_listener, handle_control_connection};
use crate::applets::core::init::services::{
    ServiceInstance, SpawnConfig, finish_daemonize, parse_environment, respawn_service,
    schedule_restart, spawn_fresh_shell, spawn_unit_command, start_forking_service, start_service,
    stop_service_instance,
};
use crate::applets::core::init::syscall::{kill_all, reboot_syscall, sync_fs};
use crate::applets::core::init::units::{
    DEFAULT_TARGET, Unit, compute_start_order, load_all_units,
};
use crate::applets::core::{LogLevel, log, log_at};
use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// 全局关机标志：SIGTERM 信号处理器设置，主循环检查。
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 全局重启标志：SIGINT 信号处理器设置，主循环检查。
static REBOOT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// self-pipe 写端 fd：信号处理器写 1 字节唤醒主循环 poll；-1 表示未创建。
static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

/// 信号处理器：SIGTERM 设置关机标志，SIGINT 设置重启标志，SIGCHLD 仅唤醒；
/// 统一写 self-pipe 通知主循环（async-signal-safe：仅原子操作 + write）。
extern "C" fn signal_handler(sig: i32) {
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

pub struct Init;
pub static INIT: &Init = &Init;

impl Applet for Init {
    fn name(&self) -> &'static str {
        "init"
    }
    fn help(&self) -> &'static str {
        "init [systemd-style] - PID 1 system initializer"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let pid = std::process::id();
        let is_pid1 = pid == 1;

        if is_pid1 {
            log("rbox init: starting as PID 1");
            install_signal_handlers();
        } else {
            log("rbox init: running in test mode (not PID 1)");
        }

        // 1. 基本环境与文件系统初始化（默认 PATH + /etc/fstab 挂载 + 主机名 + sysctl）
        setup_environment();
        mount_all_fs();
        setup_hostname();
        apply_sysctl("/etc/sysctl.conf");
        log("rbox init: basic filesystems mounted");

        // 2. 解析所有单元文件
        let units = match load_all_units() {
            Ok(u) => {
                log(&format!("rbox init: loaded {} unit(s)", u.len()));
                u
            }
            Err(e) => {
                log_at(
                    LogLevel::Error,
                    &format!("rbox init: failed to load units: {}", e),
                );
                return run_without_units();
            }
        };

        // 3. 计算从 default.target 出发的启动顺序（拓扑排序）
        let order = match compute_start_order(&units, DEFAULT_TARGET) {
            Ok(o) => {
                log_at(LogLevel::Debug, &format!("rbox init: start order: {:?}", o));
                o
            }
            Err(e) => {
                log_at(
                    LogLevel::Error,
                    &format!("rbox init: dependency error: {}", e),
                );
                return run_without_units();
            }
        };

        // 4. 依次启动服务，记录已启动的实例
        let mut services: Vec<ServiceInstance> = Vec::new();
        let mut console_child: Option<std::process::Child> = None;
        let mut console_name = String::from("console-shell.service");
        let mut console_reload: Option<String> = None;
        let mut console_cfg: Option<ConsoleConfig> = None;
        // 记录每个单元的启动结果（Requires 失败传播用）；target 恒为成功
        let mut started_ok: HashMap<String, bool> = HashMap::new();
        for unit_name in &order {
            if let Some(unit) = units.get(unit_name) {
                if unit.is_target {
                    log(&format!("rbox init: reached target {}", unit_name));
                    started_ok.insert(unit_name.clone(), true);
                    continue;
                }
                // Requires 失败传播：任一 required 单元启动失败则跳过本服务
                // （After 仅排序，不传播失败）
                if let Some(failed_dep) = failed_required_dep(unit, &started_ok) {
                    log_at(
                        LogLevel::Error,
                        &format!(
                            "rbox init: skipping {} because required unit {} failed",
                            unit_name, failed_dep
                        ),
                    );
                    started_ok.insert(unit_name.clone(), false);
                    continue;
                }
                if let Some(cmd) = &unit.service.exec_start {
                    if !unit.service.typ.is_empty()
                        && unit.service.typ != "simple"
                        && unit.service.typ != "forking"
                    {
                        log_at(
                            LogLevel::Warn,
                            &format!(
                                "rbox init: {} Type={:?} unsupported, treating as simple",
                                unit_name, unit.service.typ
                            ),
                        );
                    }
                    if !unit.service.restart.is_empty() && unit.service.restart != "on-failure" {
                        log_at(
                            LogLevel::Warn,
                            &format!(
                                "rbox init: {} Restart={:?} unsupported, ignoring",
                                unit_name, unit.service.restart
                            ),
                        );
                    }
                    if unit.unit.description.is_empty() {
                        log(&format!("rbox init: starting {}: {}", unit_name, cmd));
                    } else {
                        log(&format!(
                            "rbox init: starting {} ({}): {}",
                            unit_name, unit.unit.description, cmd
                        ));
                    }
                    let env = parse_environment(&unit.service.environment);
                    // spawn 成功与否决定 Requires 失败传播；forking 服务以父进程
                    // spawn 成功为"成功"（daemon 化结果异步，见主循环）
                    let spawned_ok = if unit.service.console {
                        console_name = unit.name.clone();
                        console_reload = unit.service.exec_reload.clone();
                        // ExecStart 是完整命令（如 /bin/rgetty -L -t 60 ttyAMA0），
                        // getty 参数直接写在配置里，init 不做额外拼接。
                        let cfg = SpawnConfig::from_unit(unit);
                        console_child = spawn_unit_command(&unit.name, cmd, &env, &cfg);
                        // 保存 respawn 配置，避免 shell 重启后丢失 Environment/LogFile/User/Group
                        console_cfg = Some(ConsoleConfig {
                            name: unit.name.clone(),
                            cmd: cmd.clone(),
                            env: env.clone(),
                            logfile: unit.service.logfile.clone(),
                            user: unit.service.user.clone(),
                            group: unit.service.group.clone(),
                        });
                        console_child.is_some()
                    } else if unit.service.typ == "forking" {
                        match start_forking_service(unit, cmd, &env) {
                            Some(inst) => {
                                services.push(inst);
                                true
                            }
                            None => false,
                        }
                    } else {
                        match start_service(unit, cmd, &env) {
                            Some(inst) => {
                                services.push(inst);
                                true
                            }
                            None => false,
                        }
                    };
                    started_ok.insert(unit_name.clone(), spawned_ok);
                } else {
                    // 无 ExecStart 的单元（如占位服务）视为启动成功
                    started_ok.insert(unit_name.clone(), true);
                }
            }
        }

        log("rbox init: startup complete");

        // 5. 主循环：回收子进程、响应控制请求，等待关机标志
        let status_listener = create_status_listener();
        reap_with_shutdown(
            console_child,
            &console_name,
            &console_reload,
            &console_cfg,
            &mut services,
            &units,
            status_listener,
        )
    }
}

/// 检查单元的 Requires 依赖中是否有启动失败的，返回第一个失败依赖名。
/// After 仅排序，不参与失败传播。
fn failed_required_dep<'a>(
    unit: &'a Unit,
    started_ok: &HashMap<String, bool>,
) -> Option<&'a String> {
    unit.unit
        .requires
        .iter()
        .find(|dep| matches!(started_ok.get(*dep), Some(false)))
}

/// console 服务的 respawn 配置（跨 respawn 保留 Environment/LogFile/User/Group）。
struct ConsoleConfig {
    name: String,
    cmd: String,
    env: Vec<(String, String)>,
    logfile: Option<String>,
    user: Option<String>,
    group: Option<String>,
}

/// console 服务 respawn：复用单元配置（Environment/LogFile/User/Group）；
/// 无配置（run_without_units 降级路径）时回退到 spawn_fresh_shell。
fn spawn_console(cfg: &Option<ConsoleConfig>) -> Option<std::process::Child> {
    match cfg {
        Some(c) => {
            let spawn_cfg = SpawnConfig {
                logfile: c.logfile.as_deref(),
                user: c.user.as_deref(),
                group: c.group.as_deref(),
            };
            spawn_unit_command(&c.name, &c.cmd, &c.env, &spawn_cfg)
        }
        None => spawn_fresh_shell(),
    }
}

/// 单元加载/依赖解析失败时的降级路径：无服务，仅拉起 console shell 并进入主循环。
fn run_without_units() -> ExitCode {
    let empty: HashMap<String, Unit> = HashMap::new();
    reap_with_shutdown(
        None,
        "console-shell.service",
        &None,
        &None,
        &mut Vec::new(),
        &empty,
        None,
    )
}

/// 安装 SIGTERM/SIGINT/SIGCHLD 信号处理器（sigaction + SA_RESTART）。
/// SIGCHLD 用于唤醒主循环收割子进程；SA_NOCLDSTOP 忽略子进程停止事件。
fn install_signal_handlers() {
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
    }
}

/// 创建 self-pipe（两端 nonblocking + close-on-exec），返回 (读端, 写端)。
fn create_signal_pipe() -> (i32, i32) {
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
fn drain_signal_pipe(fd: i32) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

/// 计算 poll 超时（毫秒）：最近的 restart 退避或 daemon 化超时；无则 -1（无限等待）。
fn compute_next_timeout(services: &[ServiceInstance]) -> i32 {
    let now = std::time::Instant::now();
    let mut earliest: Option<std::time::Duration> = None;
    for svc in services {
        if let Some(at) = svc.next_restart_at {
            let d = at.saturating_duration_since(now);
            earliest = Some(match earliest {
                Some(e) => e.min(d),
                None => d,
            });
        }
        if svc.waiting_daemonize
            && let Some(deadline) = svc.daemonize_deadline
        {
            let d = deadline.saturating_duration_since(now);
            earliest = Some(match earliest {
                Some(e) => e.min(d),
                None => d,
            });
        }
    }
    match earliest {
        Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
        None => -1,
    }
}

/// 主循环：管理 console shell（退出则 respawn）、回收/重启服务、
/// 响应 rservice/rbox status 控制请求，检测关机标志。
fn reap_with_shutdown(
    mut console: Option<std::process::Child>,
    console_name: &str,
    console_reload: &Option<String>,
    console_cfg: &Option<ConsoleConfig>,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
    status_listener: Option<UnixListener>,
) -> ExitCode {
    // 创建 self-pipe：信号处理器写 1 字节唤醒主循环 poll
    let (signal_pipe_read, signal_pipe_write) = create_signal_pipe();
    SIGNAL_PIPE_WRITE.store(signal_pipe_write, Ordering::SeqCst);

    loop {
        // 1. console shell：运行中则检查退出，退出后标记待 respawn
        if let Some(child) = console.as_mut() {
            let exited = match child.try_wait() {
                Ok(Some(_)) => true,
                // 竞态：恰在孤儿收割（waitpid -1）之后退出，状态已被取走
                Err(_) => true,
                Ok(None) => false,
            };
            if exited {
                if shutdown_requested() {
                    return do_shutdown(services);
                }
                log("rbox init: shell exited, respawning");
                console = None;
            }
        }
        // 未运行（未启动或已退出）则拉起；失败则稍后重试
        if console.is_none() && !shutdown_requested() {
            match spawn_console(console_cfg) {
                Some(c) => console = Some(c),
                None => {
                    log("rbox init: cannot spawn shell, waiting");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
            }
        }

        // 2. 回收已退出的服务进程 + forking daemon 化等待；
        //    Restart=on-failure 且非零退出时安排重启（退避 + 上限）
        for svc in services.iter_mut() {
            // 2a. forking 服务等待父进程 daemon 化（异步状态机，不阻塞主循环）
            if svc.waiting_daemonize {
                let result = svc.child.as_mut().map(|child| child.try_wait());
                match result {
                    // 父进程退出 = daemon 化完成：读 PIDFile 跟踪 daemon
                    Some(Ok(Some(_))) => finish_daemonize(svc),
                    Some(Ok(None)) => {
                        // 超时未 daemon 化：kill 并标记失败（触发 Restart 调度）
                        if svc
                            .daemonize_deadline
                            .is_some_and(|d| std::time::Instant::now() >= d)
                        {
                            if let Some(child) = svc.child.as_mut() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            log_at(
                                LogLevel::Warn,
                                &format!(
                                    "rbox init: {} did not daemonize within {}s, killing",
                                    svc.name, svc.timeout_start_sec
                                ),
                            );
                            svc.child = None;
                            svc.waiting_daemonize = false;
                            svc.daemonize_deadline = None;
                            schedule_restart(svc, true);
                        }
                    }
                    // 竞态：状态已被孤儿收割取走，视为 daemon 化完成
                    Some(Err(_)) => finish_daemonize(svc),
                    // child 已被 reap_orphans 处理（finish_daemonize 已调用）
                    None => {}
                }
                continue;
            }
            if let Some(child) = svc.child.as_mut() {
                let (exited, failed) = match child.try_wait() {
                    Ok(Some(status)) => {
                        log_at(
                            LogLevel::Warn,
                            &format!(
                                "rbox init: service {} exited (code {:?})",
                                svc.name,
                                status.code()
                            ),
                        );
                        (true, !status.success())
                    }
                    // 竞态：状态已被孤儿收割取走，视为退出但不触发重启
                    Err(_) => (true, false),
                    Ok(None) => (false, false),
                };
                if exited {
                    svc.child = None;
                    schedule_restart(svc, failed);
                }
            }
            // 2b. 到达 RestartSec 退避时间点则重新拉起
            if let Some(at) = svc.next_restart_at
                && std::time::Instant::now() >= at
                && !shutdown_requested()
                && !svc.stopped
            {
                svc.next_restart_at = None;
                log_at(
                    LogLevel::Info,
                    &format!(
                        "rbox init: restarting {} (attempt {})",
                        svc.name,
                        svc.fail_count + 1
                    ),
                );
                respawn_service(svc);
            }
        }

        // 2.1 收割收养的孤儿进程（waitpid -1），防止僵尸累积
        reap_orphans(services, &mut console);

        // 3. 关机/重启标志
        if shutdown_requested() {
            log("rbox init: shutdown requested, terminating shell");
            if let Some(mut child) = console.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return do_shutdown(services);
        }

        // 4. 事件等待：poll 监听 self-pipe 与 status socket。
        //    超时为最近的 restart 退避 / daemon 化超时（无定时则无限等待，纯事件驱动）。
        let timeout = compute_next_timeout(services);
        let status_fd = status_listener.as_ref().map(|l| l.as_raw_fd());
        let mut fds = [
            libc::pollfd {
                fd: signal_pipe_read,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: status_fd.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let nfds = if status_fd.is_some() { 2 } else { 1 };
        let n = unsafe { libc::poll(fds.as_mut_ptr(), nfds as libc::nfds_t, timeout) };
        if n > 0 {
            if fds[0].revents & libc::POLLIN != 0 {
                drain_signal_pipe(signal_pipe_read);
            }
            // 响应控制请求（rbox status / rservice）
            if fds[1].revents & libc::POLLIN != 0
                && let Some(listener) = &status_listener
                && let Ok((stream, _)) = listener.accept()
            {
                handle_control_connection(
                    stream,
                    console_name,
                    console_reload,
                    console.as_ref(),
                    services,
                    units,
                );
            }
        } else if n < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            // 非 EINTR 的 poll 错误：短暂休眠避免忙循环
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

/// 收割收养的孤儿进程（waitpid -1, WNOHANG），防止僵尸累积。
/// 已知服务/console 的子进程由 try_wait 先行处理；
/// 若恰在 try_wait 之后退出被这里收割（竞态），同步其状态。
fn reap_orphans(services: &mut [ServiceInstance], console: &mut Option<std::process::Child>) {
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break; // 0 = 无已退出子进程，-1 = 无子进程
        }
        let pid = pid as u32;
        if let Some(c) = console.as_mut()
            && c.id() == pid
        {
            log("rbox init: console shell reaped");
            *console = None;
            continue;
        }
        for svc in services.iter_mut() {
            // forking daemon 退出（被收养的 daemon 由这里匹配并触发重启调度）
            if svc.tracked_pid == Some(pid) {
                log_at(
                    LogLevel::Warn,
                    &format!("rbox init: service {} (daemon) exited", svc.name),
                );
                svc.tracked_pid = None;
                let failed = !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0;
                schedule_restart(svc, failed);
                break;
            }
            if let Some(child) = svc.child.as_mut()
                && child.id() == pid
            {
                if svc.waiting_daemonize {
                    // forking 父进程被孤儿收割收割：同样完成 daemon 化
                    finish_daemonize(svc);
                } else {
                    log(&format!("rbox init: service {} reaped", svc.name));
                    svc.child = None;
                }
                break;
            }
        }
    }
}

/// 执行有序关机：逆序停止服务，杀残留进程，再 power off。
fn do_shutdown(services: &mut [ServiceInstance]) -> ExitCode {
    log("rbox init: shutting down");
    for svc in services.iter_mut().rev() {
        stop_service_instance(svc);
    }
    log("rbox init: sending SIGTERM to all processes");
    let _ = kill_all(libc::SIGTERM);
    // 等待所有子进程退出（最多 5 秒），实现优雅关机；超时继续关机流程
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid < 0 {
            break; // ECHILD：无子进程
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        if pid == 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // pid > 0：已收割一个，立即继续收割其余
    }
    sync_fs();
    let is_reboot = REBOOT_REQUESTED.load(Ordering::SeqCst);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::core::init::services::test_svc;

    /// 从 TOML 构造一个带 Requires 列表的测试单元。
    fn unit_with_requires(requires: &[&str]) -> Unit {
        let quoted = requires
            .iter()
            .map(|r| format!("\"{}\"", r))
            .collect::<Vec<_>>()
            .join(", ");
        let mut u: Unit =
            toml::from_str(&format!("[Unit]\nName = \"t\"\nRequires = [{}]\n", quoted)).unwrap();
        u.name = "t".to_string();
        u
    }

    #[test]
    fn failed_required_dep_detects_failure() {
        let u = unit_with_requires(&["a.service", "b.service"]);
        let mut ok = HashMap::new();
        ok.insert("a.service".to_string(), true);
        ok.insert("b.service".to_string(), false);
        assert_eq!(failed_required_dep(&u, &ok), Some(&"b.service".to_string()));
    }

    #[test]
    fn failed_required_dep_all_ok() {
        let u = unit_with_requires(&["a.service"]);
        let mut ok = HashMap::new();
        ok.insert("a.service".to_string(), true);
        assert_eq!(failed_required_dep(&u, &ok), None);
    }

    #[test]
    fn failed_required_dep_missing_dep_is_not_failure() {
        // 缺失依赖（不在 started_ok）不算失败——已在拓扑阶段告警
        let u = unit_with_requires(&["ghost.service"]);
        let ok = HashMap::new();
        assert_eq!(failed_required_dep(&u, &ok), None);
    }

    #[test]
    fn timeout_infinite_without_timers() {
        let services = vec![test_svc("a.service", false)];
        assert_eq!(compute_next_timeout(&services), -1);
    }

    #[test]
    fn timeout_uses_earliest_restart() {
        let mut svc = test_svc("a.service", true);
        svc.next_restart_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
        let t = compute_next_timeout(&[svc]);
        assert!((1800..=2000).contains(&t), "timeout={t}");
    }

    #[test]
    fn timeout_uses_daemonize_deadline() {
        let mut svc = test_svc("a.service", false);
        svc.waiting_daemonize = true;
        svc.daemonize_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
        let t = compute_next_timeout(&[svc]);
        assert!((400..=500).contains(&t), "timeout={t}");
    }
}
