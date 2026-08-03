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
    ServiceInstance, SpawnConfig, parse_environment, respawn_service, schedule_restart,
    spawn_fresh_shell, spawn_unit_command, start_forking_service, start_service,
    stop_service_instance,
};
use crate::applets::core::init::syscall::{kill_all, reboot_syscall, sync_fs};
use crate::applets::core::init::units::{
    DEFAULT_TARGET, Unit, compute_start_order, load_all_units,
};
use crate::applets::core::log;
use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

/// 全局关机标志：SIGTERM 信号处理器设置，主循环检查。
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 全局重启标志：SIGINT 信号处理器设置，主循环检查。
static REBOOT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 信号处理器：SIGTERM 设置关机标志，SIGINT 设置重启标志。
extern "C" fn signal_handler(sig: i32) {
    if sig == libc::SIGINT {
        REBOOT_REQUESTED.store(true, Ordering::SeqCst);
    } else {
        SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
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
                log(&format!("rbox init: failed to load units: {}", e));
                let empty: HashMap<String, Unit> = HashMap::new();
                return reap_with_shutdown(
                    None,
                    "console-shell.service",
                    &None,
                    &mut Vec::new(),
                    &empty,
                    None,
                );
            }
        };

        // 3. 计算从 default.target 出发的启动顺序（拓扑排序）
        let order = match compute_start_order(&units, DEFAULT_TARGET) {
            Ok(o) => {
                log(&format!("rbox init: start order: {:?}", o));
                o
            }
            Err(e) => {
                log(&format!("rbox init: dependency error: {}", e));
                let empty: HashMap<String, Unit> = HashMap::new();
                return reap_with_shutdown(
                    None,
                    "console-shell.service",
                    &None,
                    &mut Vec::new(),
                    &empty,
                    None,
                );
            }
        };

        // 4. 依次启动服务，记录已启动的实例
        let mut services: Vec<ServiceInstance> = Vec::new();
        let mut console_child: Option<std::process::Child> = None;
        let mut console_name = String::from("console-shell.service");
        let mut console_reload: Option<String> = None;
        for unit_name in &order {
            if let Some(unit) = units.get(unit_name) {
                if unit.is_target {
                    log(&format!("rbox init: reached target {}", unit_name));
                    continue;
                }
                if let Some(cmd) = &unit.service.exec_start {
                    if !unit.service.typ.is_empty()
                        && unit.service.typ != "simple"
                        && unit.service.typ != "forking"
                    {
                        log(&format!(
                            "rbox init: warning: {} Type={:?} unsupported, treating as simple",
                            unit_name, unit.service.typ
                        ));
                    }
                    if !unit.service.restart.is_empty() && unit.service.restart != "on-failure" {
                        log(&format!(
                            "rbox init: warning: {} Restart={:?} unsupported, treating as no",
                            unit_name, unit.service.restart
                        ));
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
                    if unit.service.console {
                        console_name = unit.name.clone();
                        console_reload = unit.service.exec_reload.clone();
                        let cfg = SpawnConfig::from_unit(unit);
                        console_child = spawn_unit_command(&unit.name, cmd, &env, &cfg);
                    } else if unit.service.typ == "forking" {
                        if let Some(inst) = start_forking_service(unit, cmd, &env) {
                            services.push(inst);
                        }
                    } else if let Some(inst) = start_service(unit, cmd, &env) {
                        services.push(inst);
                    }
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
            &mut services,
            &units,
            status_listener,
        )
    }
}

/// 安装 SIGTERM/SIGINT 信号处理器（sigaction + SA_RESTART）。
fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = signal_handler as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

/// 主循环：管理 console shell（退出则 respawn）、回收/重启服务、
/// 响应 rservice/rbox status 控制请求，检测关机标志。
fn reap_with_shutdown(
    mut console: Option<std::process::Child>,
    console_name: &str,
    console_reload: &Option<String>,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
    status_listener: Option<UnixListener>,
) -> ExitCode {
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
            match spawn_fresh_shell() {
                Some(c) => console = Some(c),
                None => {
                    log("rbox init: cannot spawn shell, waiting");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
            }
        }

        // 2. 回收已退出的服务进程；Restart=on-failure 且非零退出时安排重启（退避 + 上限）
        for svc in services.iter_mut() {
            if let Some(child) = svc.child.as_mut() {
                let (exited, failed) = match child.try_wait() {
                    Ok(Some(status)) => {
                        log(&format!(
                            "rbox init: service {} exited (code {:?})",
                            svc.name,
                            status.code()
                        ));
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
            // 2a. 到达 RestartSec 退避时间点则重新拉起
            if let Some(at) = svc.next_restart_at
                && std::time::Instant::now() >= at
                && !shutdown_requested()
                && !svc.stopped
            {
                svc.next_restart_at = None;
                log(&format!(
                    "rbox init: restarting {} (Restart=on-failure, attempt {})",
                    svc.name,
                    svc.fail_count + 1
                ));
                respawn_service(svc);
            }
        }

        // 2.1 收割收养的孤儿进程（waitpid -1），防止僵尸累积
        reap_orphans(services, &mut console);

        // 2.5 响应控制请求（rbox status / rservice）
        if let Some(listener) = &status_listener
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

        // 3. 关机/重启标志
        if shutdown_requested() {
            log("rbox init: shutdown requested, terminating shell");
            if let Some(mut child) = console.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return do_shutdown(services);
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
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
                log(&format!("rbox init: service {} (daemon) exited", svc.name));
                svc.tracked_pid = None;
                let failed = !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0;
                schedule_restart(svc, failed);
                break;
            }
            if let Some(child) = svc.child.as_mut()
                && child.id() == pid
            {
                log(&format!("rbox init: service {} reaped", svc.name));
                svc.child = None;
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
    std::thread::sleep(std::time::Duration::from_millis(500));
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
        log(&format!("rbox init: reboot syscall failed: {}", e));
        // 重启失败时回退到关机；仍失败则挂起等待人工干预
        if is_reboot {
            let _ = reboot_syscall(libc::RB_POWER_OFF);
        }
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
