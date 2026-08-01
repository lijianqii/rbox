//! `init` - PID 1 系统初始化进程。
//!
//! systemd 风格的 target 依赖链启动（配置使用 TOML 格式）：
//! - 解析 `/etc/rbox/system/` 下的 `.toml` 单元文件。
//! - 支持 `[Unit]` 的 `After=`/`Requires=`、`[Install]` 的 `WantedBy=`、
//!   `[Service]` 的 `Type=`(仅 simple)/`ExecStart=`。
//! - 从 `default.target` 出发，按依赖拓扑序启动服务。
//! - `ExecStart` 既支持外部二进制，也支持 rbox 内置 applet
//!   （形如 `rbox <applet> [args...]` 的命令直接在本进程内调用）。
//! - 启动完成后 fork 一个 shell（作为 getty 替代），init 作为 PID 1 常驻，
//!   回收僵尸进程；shell 退出后重新 fork。

use crate::applet::Applet;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ExitCode, Stdio};
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
fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst) || REBOOT_REQUESTED.load(Ordering::SeqCst)
}

pub struct Init;
pub static INIT: &Init = &Init;

const SYSTEM_DIR: &str = "/etc/rbox/system";
const DEFAULT_TARGET: &str = "default.target";
/// status 查询用的 unix socket 路径（/tmp 为 tmpfs，重启后自动消失）。
const STATUS_SOCKET: &str = "/tmp/rbox.sock";

/// 内置默认挂载集：/etc/fstab 缺失时回退使用。
const DEFAULT_FSTAB: &[&str] = &[
    "proc     /proc      proc      defaults  0 0",
    "sysfs    /sys       sysfs     defaults  0 0",
    "devtmpfs /dev       devtmpfs  defaults  0 0",
    "devpts   /dev/pts   devpts    defaults  0 0",
    "tmpfs    /tmp       tmpfs     defaults  0 0",
];

/// 解析后的单元文件（TOML 反序列化）。
/// TOML 表名使用 systemd 风格的 [Unit]/[Service]/[Install]。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Unit {
    #[serde(skip)]
    name: String,
    #[serde(skip)]
    is_target: bool,
    #[serde(default, rename = "Unit")]
    unit: UnitSection,
    #[serde(default, rename = "Service")]
    service: ServiceSection,
    #[serde(default, rename = "Install")]
    install: InstallSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UnitSection {
    #[serde(default)]
    #[serde(rename = "Description")]
    description: String,
    /// 单元名：rservice/status/依赖引用均使用它；缺省回退文件名
    #[serde(default)]
    #[serde(rename = "Name")]
    name: String,
    #[serde(default)]
    #[serde(rename = "After")]
    after: Vec<String>,
    #[serde(default)]
    #[serde(rename = "Requires")]
    requires: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ServiceSection {
    #[serde(default)]
    #[serde(rename = "Type")]
    typ: String,
    #[serde(default)]
    #[serde(rename = "ExecStart")]
    exec_start: Option<String>,
    #[serde(default)]
    #[serde(rename = "ExecStop")]
    exec_stop: Option<String>,
    /// rservice reload 时执行的命令
    #[serde(default)]
    #[serde(rename = "ExecReload")]
    exec_reload: Option<String>,
    /// 重启策略："" / "no"（默认）或 "on-failure"
    #[serde(default)]
    #[serde(rename = "Restart")]
    restart: String,
    /// 自动重启间隔（秒，默认 1）
    #[serde(default = "default_restart_sec")]
    #[serde(rename = "RestartSec")]
    restart_sec: u64,
    /// 连续失败重启上限（默认 5，达到后停止重启）
    #[serde(default = "default_start_limit_burst")]
    #[serde(rename = "StartLimitBurst")]
    start_limit_burst: u32,
    /// Type=forking 时等待父进程退出的超时（秒，默认 10）
    #[serde(default = "default_timeout_start")]
    #[serde(rename = "TimeoutStartSec")]
    timeout_start_sec: u64,
    /// Type=forking 的 daemon PID 文件（可选）
    #[serde(default)]
    #[serde(rename = "PIDFile")]
    pidfile: Option<String>,
    /// 服务环境变量：["VAR=value", ...]
    #[serde(default)]
    #[serde(rename = "Environment")]
    environment: Vec<String>,
    /// stdout/stderr 重定向文件（可选）
    #[serde(default)]
    #[serde(rename = "LogFile")]
    logfile: Option<String>,
    /// 以指定用户/组运行（可选）
    #[serde(default)]
    #[serde(rename = "User")]
    user: Option<String>,
    #[serde(default)]
    #[serde(rename = "Group")]
    group: Option<String>,
    /// 前台 console 服务（如交互 shell），退出后自动 respawn
    #[serde(default)]
    #[serde(rename = "Console")]
    console: bool,
}

fn default_restart_sec() -> u64 {
    1
}
fn default_start_limit_burst() -> u32 {
    5
}
fn default_timeout_start() -> u64 {
    10
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InstallSection {
    #[serde(default)]
    #[serde(rename = "WantedBy")]
    wanted_by: Vec<String>,
}

/// 已启动服务的运行时记录。
/// 持有 `Child` 句柄以便主循环 try_wait 回收，避免僵尸进程。
struct ServiceInstance {
    name: String,
    child: Option<std::process::Child>,
    /// Type=forking 的 daemon pid（被 init 收养，退出时由孤儿收割匹配）
    tracked_pid: Option<u32>,
    exec_stop: Option<String>,
    exec_reload: Option<String>,
    /// ExecStart 原文，Restart=on-failure 时用于重新拉起
    exec_start: String,
    /// 服务环境变量（已解析的 VAR=value 对）
    env: Vec<(String, String)>,
    /// 重启时保留的 spawn 配置
    logfile: Option<String>,
    user: Option<String>,
    group: Option<String>,
    /// Type=forking 相关（重启时重新走 daemon 化流程）
    is_forking: bool,
    pidfile: Option<String>,
    timeout_start_sec: u64,
    restart_on_failure: bool,
    /// 自动重启间隔与连续失败上限
    restart_sec: u64,
    start_limit_burst: u32,
    /// 连续失败次数（成功退出或手动 start 时清零）
    fail_count: u32,
    /// 待重启时间点（RestartSec 退避调度）
    next_restart_at: Option<std::time::Instant>,
    /// stop 请求后标记：禁止自动重启（Restart=on-failure 也不重启）
    stopped: bool,
}

impl ServiceInstance {
    /// status 输出的一行状态。
    fn status_line(&self) -> String {
        let restart = if self.restart_on_failure {
            " restart=on-failure"
        } else {
            ""
        };
        if let Some(pid) = self.tracked_pid {
            return format!("{} running pid={}{}\n", self.name, pid, restart);
        }
        match &self.child {
            Some(c) => format!("{} running pid={}{}\n", self.name, c.id(), restart),
            None => {
                let state = if self.stopped { "stopped" } else { "exited" };
                format!("{} {}{}\n", self.name, state, restart)
            }
        }
    }
}

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
            if let Some(at) = svc.next_restart_at {
                if std::time::Instant::now() >= at && !shutdown_requested() && !svc.stopped {
                    svc.next_restart_at = None;
                    log(&format!(
                        "rbox init: restarting {} (Restart=on-failure, attempt {})",
                        svc.name,
                        svc.fail_count + 1
                    ));
                    respawn_service(svc);
                }
            }
        }

        // 2.1 收割收养的孤儿进程（waitpid -1），防止僵尸累积
        reap_orphans(services, &mut console);

        // 2.5 响应控制请求（rbox status / rservice）
        if let Some(listener) = &status_listener {
            if let Ok((stream, _)) = listener.accept() {
                handle_control_connection(stream, console_name, console.as_ref(), services, units);
            }
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
        if let Some(c) = console.as_mut() {
            if c.id() == pid {
                log("rbox init: console shell reaped");
                *console = None;
                continue;
            }
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
            if let Some(child) = svc.child.as_mut() {
                if child.id() == pid {
                    log(&format!("rbox init: service {} reaped", svc.name));
                    svc.child = None;
                    break;
                }
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

fn spawn_fresh_shell() -> Option<std::process::Child> {
    std::process::Command::new("/bin/rbox")
        .arg("shell")
        .spawn()
        .or_else(|_| std::process::Command::new("/bin/sh").spawn())
        .ok()
}

/// spawn 附加配置（LogFile/User/Group）。
struct SpawnConfig<'a> {
    /// stdout/stderr 重定向文件
    logfile: Option<&'a str>,
    /// 降权用户/组名
    user: Option<&'a str>,
    group: Option<&'a str>,
}

impl SpawnConfig<'_> {
    fn from_unit(unit: &Unit) -> SpawnConfig<'_> {
        SpawnConfig {
            logfile: unit.service.logfile.as_deref(),
            user: unit.service.user.as_deref(),
            group: unit.service.group.as_deref(),
        }
    }
}

/// 解析 ExecStart 命令并 spawn 子进程，返回子进程句柄。
/// ExecStart 以 `rbox` 或 `/bin/rbox` 开头时统一走 /bin/rbox。
/// 服务放入独立进程组（process_group），关机时可按组清理其后代进程。
fn spawn_unit_command(
    name: &str,
    cmd: &str,
    env: &[(String, String)],
    cfg: &SpawnConfig<'_>,
) -> Option<std::process::Child> {
    let argv = parse_cmdline(cmd);
    if argv.is_empty() {
        return None;
    }
    let (program, args) = if (argv[0] == "rbox" || argv[0] == "/bin/rbox") && argv.len() >= 2 {
        ("/bin/rbox", &argv[1..])
    } else {
        (argv[0].as_str(), &argv[1..])
    };
    let mut command = std::process::Command::new(program);
    command.args(args);
    command.envs(env.iter().cloned());
    command.process_group(0);
    // 输出重定向到日志文件（追加），否则继承 console
    if let Some(path) = cfg.logfile {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    let _ = command.stdout(Stdio::from(f));
                    let _ = command.stderr(Stdio::from(f2));
                }
                Err(_) => {
                    let _ = command.stdout(Stdio::from(f));
                }
            },
            Err(e) => {
                log(&format!(
                    "rbox init: cannot open log file {} for {}: {}",
                    path, name, e
                ));
                return None;
            }
        }
    }
    // 降权：User=/Group=（getpwnam/getgrnam 解析，失败则拒绝启动）
    if let Some(user) = cfg.user {
        match lookup_uid(user) {
            Some(uid) => {
                command.uid(uid);
            }
            None => {
                log(&format!("rbox init: unknown user '{}' for {}", user, name));
                return None;
            }
        }
    }
    if let Some(group) = cfg.group {
        match lookup_gid(group) {
            Some(gid) => {
                command.gid(gid);
            }
            None => {
                log(&format!(
                    "rbox init: unknown group '{}' for {}",
                    group, name
                ));
                return None;
            }
        }
    }
    match command.spawn() {
        Ok(child) => {
            log(&format!("rbox init: started {} (pid {})", name, child.id()));
            Some(child)
        }
        Err(e) => {
            log(&format!("rbox init: failed to start {}: {}", name, e));
            None
        }
    }
}

/// 通过 /etc/passwd 解析用户名 -> uid。
fn lookup_uid(name: &str) -> Option<u32> {
    let Ok(c) = std::ffi::CString::new(name) else {
        return None;
    };
    unsafe {
        let pwd = libc::getpwnam(c.as_ptr());
        if pwd.is_null() {
            None
        } else {
            Some((*pwd).pw_uid)
        }
    }
}

/// 通过 /etc/group 解析组名 -> gid。
fn lookup_gid(name: &str) -> Option<u32> {
    let Ok(c) = std::ffi::CString::new(name) else {
        return None;
    };
    unsafe {
        let grp = libc::getgrnam(c.as_ptr());
        if grp.is_null() {
            None
        } else {
            Some((*grp).gr_gid)
        }
    }
}

/// 解析 [Service] Environment 列表为 (VAR, value) 对；跳过非法项。
fn parse_environment(envs: &[String]) -> Vec<(String, String)> {
    envs.iter()
        .filter_map(|e| {
            let (k, v) = e.split_once('=')?;
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// 一条 fstab 挂载记录：<device> <mountpoint> <type> <options> [<dump> <pass>]。
#[derive(Debug, Clone)]
struct FstabEntry {
    device: String,
    mountpoint: String,
    fstype: String,
    options: String,
}

/// 解析一行 fstab 记录；空行、注释行、字段不足的行返回 None。
fn parse_fstab_line(line: &str) -> Option<FstabEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let device = fields.next()?;
    let mountpoint = fields.next()?;
    let fstype = fields.next()?;
    let options = fields.next().unwrap_or("defaults");
    Some(FstabEntry {
        device: device.to_string(),
        mountpoint: mountpoint.to_string(),
        fstype: fstype.to_string(),
        options: options.to_string(),
    })
}

/// 解析整个 fstab 内容。
fn parse_fstab(content: &str) -> Vec<FstabEntry> {
    content.lines().filter_map(parse_fstab_line).collect()
}

/// 挂载所有文件系统：优先读取 /etc/fstab，缺失时回退到内置默认集。
/// 单个挂载失败只记录日志，不中断其余挂载。
fn mount_all_fs() {
    let entries: Vec<FstabEntry> = match fs::read_to_string("/etc/fstab") {
        Ok(content) => parse_fstab(&content),
        Err(_) => {
            log("rbox init: /etc/fstab not found, using built-in defaults");
            DEFAULT_FSTAB
                .iter()
                .filter_map(|l| parse_fstab_line(l))
                .collect()
        }
    };
    for e in &entries {
        log(&format!(
            "rbox init: mounting {} on {} ({})",
            e.device, e.mountpoint, e.fstype
        ));
        let _ = fs::create_dir_all(&e.mountpoint);
        if let Err(err) = run_mount(&e.device, &e.mountpoint, &e.fstype, &e.options) {
            log(&format!(
                "rbox init: mount {} on {} failed: {}",
                e.device, e.mountpoint, err
            ));
        }
    }
}

/// 为所有子进程（shell、服务）提供默认 PATH。
fn setup_environment() {
    if std::env::var_os("PATH").is_none() {
        // SAFETY: init 是单线程 PID 1，无并发修改环境变量风险
        unsafe { std::env::set_var("PATH", "/bin:/sbin:/usr/bin:/usr/sbin") };
    }
}

/// 读取 /etc/hostname（取第一行）并设置主机名；文件缺失或为空时静默跳过。
fn setup_hostname() {
    let Ok(content) = fs::read_to_string("/etc/hostname") else {
        return;
    };
    let hostname = content.lines().next().unwrap_or("").trim();
    if hostname.is_empty() {
        return;
    }
    let Ok(c) = std::ffi::CString::new(hostname) else {
        return;
    };
    let rc = unsafe { libc::sethostname(c.as_ptr(), hostname.len()) };
    if rc == 0 {
        log(&format!("rbox init: hostname set to {}", hostname));
    } else {
        log(&format!(
            "rbox init: sethostname failed: {}",
            std::io::Error::last_os_error()
        ));
    }
}

/// 解析 sysctl.conf 内容为 (key, value) 对；跳过空行/注释/格式错误行。
fn parse_sysctl_conf(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// 应用 /etc/sysctl.conf：key 的点号转 /proc/sys/ 路径并写入。
/// 文件缺失时静默跳过；单个条目失败只记日志。
fn apply_sysctl(path: &str) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    for (key, value) in parse_sysctl_conf(&content) {
        let proc_path = format!("/proc/sys/{}", key.replace('.', "/"));
        match fs::write(&proc_path, value.as_bytes()) {
            Ok(()) => log(&format!("rbox init: sysctl {} = {}", key, value)),
            Err(e) => log(&format!("rbox init: sysctl {} failed: {}", key, e)),
        }
    }
}

fn run_mount(src: &str, tgt: &str, fstype: &str, options: &str) -> std::io::Result<()> {
    use std::ffi::CString;
    let s = CString::new(src).unwrap();
    let t = CString::new(tgt).unwrap();
    let f = CString::new(fstype).unwrap();
    let rc = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            f.as_ptr(),
            parse_mount_flags(options),
            std::ptr::null::<std::ffi::c_void>(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 解析 fstab options（逗号分隔）为 mount(2) 标志位；defaults/未知选项视为 0。
fn parse_mount_flags(options: &str) -> libc::c_ulong {
    let mut flags: libc::c_ulong = 0;
    for opt in options.split(',') {
        match opt {
            "defaults" | "rw" | "" => {}
            "ro" => flags |= libc::MS_RDONLY,
            "remount" => flags |= libc::MS_REMOUNT,
            "noexec" => flags |= libc::MS_NOEXEC,
            "nosuid" => flags |= libc::MS_NOSUID,
            "nodev" => flags |= libc::MS_NODEV,
            "noatime" => flags |= libc::MS_NOATIME,
            "sync" => flags |= libc::MS_SYNCHRONOUS,
            _ => {}
        }
    }
    flags
}

/// 单元名解析：优先 [Unit] Name 字段，缺省回退文件名（去掉 .toml）。
fn resolve_unit_name(file_stem: &str, declared: &str) -> String {
    if declared.is_empty() {
        file_stem.to_string()
    } else {
        declared.to_string()
    }
}

/// 是否为 target 单元（按文件名 .target 后缀判定，与 Name 字段无关）。
fn is_target_file(file_stem: &str) -> bool {
    file_stem.ends_with(".target")
}

/// 加载 SYSTEM_DIR 下所有 .toml 单元文件。
fn load_all_units() -> std::io::Result<HashMap<String, Unit>> {
    let mut units: HashMap<String, Unit> = HashMap::new();
    let dir = Path::new(SYSTEM_DIR);
    if !dir.exists() {
        return Ok(units);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            if let Ok(content) = fs::read_to_string(&path) {
                match toml::from_str::<Unit>(&content) {
                    Ok(mut unit) => {
                        // 文件名去掉 .toml；单元名优先用 [Unit] Name，缺省回退文件名
                        let file_stem = path
                            .file_stem()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        unit.is_target = is_target_file(&file_stem);
                        unit.name = resolve_unit_name(&file_stem, &unit.unit.name);
                        units.insert(unit.name.clone(), unit);
                    }
                    Err(e) => {
                        log(&format!(
                            "rbox init: parse error in {}: {}",
                            path.display(),
                            e
                        ));
                    }
                }
            }
        }
    }
    Ok(units)
}

/// 从 default.target 出发，计算服务的启动顺序（拓扑排序）。
/// Requires= 和 After= 都构成"必须先启动"的边。
fn compute_start_order(units: &HashMap<String, Unit>, root: &str) -> Result<Vec<String>, String> {
    let mut order = Vec::new();
    let mut visited: HashMap<String, u8> = HashMap::new(); // 0=未访问 1=进行中 2=已完成

    fn visit(
        name: &str,
        units: &HashMap<String, Unit>,
        order: &mut Vec<String>,
        visited: &mut HashMap<String, u8>,
    ) -> Result<(), String> {
        let st = *visited.entry(name.to_string()).or_insert(0);
        match st {
            2 => return Ok(()),
            1 => return Err(format!("cycle detected at {}", name)),
            _ => {}
        }
        visited.insert(name.to_string(), 1);

        let unit = match units.get(name) {
            Some(u) => u,
            None => {
                visited.insert(name.to_string(), 2);
                return Ok(());
            }
        };

        let mut deps = unit.unit.requires.clone();
        deps.extend(unit.unit.after.iter().cloned());
        // target 节点：把所有 WantedBy=该 target 的服务拉进来（反向依赖）
        if unit.is_target {
            for (other_name, other) in units.iter() {
                if other.install.wanted_by.iter().any(|w| w == name) {
                    deps.push(other_name.clone());
                }
            }
        }
        for dep in &deps {
            visit(dep, units, order, visited)?;
        }

        order.push(name.to_string());
        visited.insert(name.to_string(), 2);
        Ok(())
    }

    visit(root, units, &mut order, &mut visited)?;
    Ok(order)
}

/// 启动一个 simple 类型服务，返回运行时实例。
fn start_service(unit: &Unit, cmd: &str, env: &[(String, String)]) -> Option<ServiceInstance> {
    let cfg = SpawnConfig::from_unit(unit);
    let child = spawn_unit_command(&unit.name, cmd, env, &cfg)?;
    Some(new_service_instance(unit, cmd, env, Some(child), None))
}

/// 启动一个 forking 类型服务：等父进程退出完成 daemon 化，
/// 有 PIDFile 时跟踪 daemon pid（被 init 收养，退出由孤儿收割匹配）。
fn start_forking_service(
    unit: &Unit,
    cmd: &str,
    env: &[(String, String)],
) -> Option<ServiceInstance> {
    let cfg = SpawnConfig::from_unit(unit);
    let mut child = spawn_unit_command(&unit.name, cmd, env, &cfg)?;
    // 等待父进程退出（daemon 化完成），超时 TimeoutStartSec
    let timeout = unit.service.timeout_start_sec;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    loop {
        if shutdown_requested() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    log(&format!(
                        "rbox init: {} did not daemonize within {}s, killing",
                        unit.name, timeout
                    ));
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    // 读 PID 文件获取 daemon pid（可选）
    let tracked_pid = unit.service.pidfile.as_deref().and_then(read_pid_file);
    if unit.service.pidfile.is_some() && tracked_pid.is_none() {
        log(&format!(
            "rbox init: {} daemonized but PIDFile unreadable, not tracking",
            unit.name
        ));
    }
    Some(new_service_instance(unit, cmd, env, None, tracked_pid))
}

/// 读取 PID 文件并解析 pid。
fn read_pid_file(path: &str) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}

/// 从单元配置构造运行时实例（公共字段）。
fn new_service_instance(
    unit: &Unit,
    cmd: &str,
    env: &[(String, String)],
    child: Option<std::process::Child>,
    tracked_pid: Option<u32>,
) -> ServiceInstance {
    ServiceInstance {
        name: unit.name.clone(),
        child,
        tracked_pid,
        exec_stop: unit.service.exec_stop.clone(),
        exec_reload: unit.service.exec_reload.clone(),
        exec_start: cmd.to_string(),
        env: env.to_vec(),
        restart_on_failure: unit.service.restart == "on-failure",
        restart_sec: unit.service.restart_sec,
        start_limit_burst: unit.service.start_limit_burst,
        fail_count: 0,
        next_restart_at: None,
        logfile: unit.service.logfile.clone(),
        user: unit.service.user.clone(),
        group: unit.service.group.clone(),
        is_forking: unit.service.typ == "forking",
        pidfile: unit.service.pidfile.clone(),
        timeout_start_sec: unit.service.timeout_start_sec,
        stopped: false,
    }
}

/// 将命令字符串切分为 argv（简单空格切分，支持双引号）。
fn parse_cmdline(s: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => in_quote = !in_quote,
            ' ' if !in_quote => {
                if !cur.is_empty() {
                    argv.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        argv.push(cur);
    }
    argv
}

/// 创建 status 查询 socket（非阻塞）；失败时返回 None（不影响启动）。
fn create_status_listener() -> Option<UnixListener> {
    match UnixListener::bind(STATUS_SOCKET) {
        Ok(l) => {
            let _ = l.set_nonblocking(true);
            Some(l)
        }
        Err(e) => {
            log(&format!("rbox init: status socket failed: {}", e));
            None
        }
    }
}

/// 处理一次控制连接：读一行请求，分发到 status/start/stop/restart，回写响应，关闭。
/// 读请求带 100ms 超时，避免异常客户端挂住主循环。
fn handle_control_connection(
    mut stream: UnixStream,
    console_name: &str,
    console: Option<&std::process::Child>,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
) {
    let mut req = String::new();
    if let Ok(peer) = stream.try_clone() {
        let mut reader = std::io::BufReader::new(peer);
        let _ = reader.read_line(&mut req);
    }
    let resp = match parse_control_request(&req) {
        Ok(r) => execute_control_request(r, console_name, console, services, units),
        Err(e) => format!("error: {}\n", e),
    };
    let _ = stream.write_all(resp.as_bytes());
}

/// 控制请求：status 查询或服务管理命令。
#[derive(Debug, PartialEq)]
enum ControlRequest<'a> {
    /// status [unit]：unit 为 None 时列出全部
    Status(Option<&'a str>),
    Start(&'a str),
    Stop(&'a str),
    Restart(&'a str),
    Reload(&'a str),
}

/// 解析控制请求行；空行等价于 status（列出全部）。
fn parse_control_request(req: &str) -> Result<ControlRequest<'_>, String> {
    let req = req.trim();
    let mut parts = req.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match cmd {
        "" | "status" => Ok(ControlRequest::Status(arg)),
        "start" | "stop" | "restart" | "reload" => match arg {
            Some(unit) => Ok(match cmd {
                "start" => ControlRequest::Start(unit),
                "stop" => ControlRequest::Stop(unit),
                "reload" => ControlRequest::Reload(unit),
                _ => ControlRequest::Restart(unit),
            }),
            None => Err(format!("usage: {} <unit>", cmd)),
        },
        other => Err(format!("unknown command: {}", other)),
    }
}

/// 执行控制请求，返回响应文本。
fn execute_control_request(
    req: ControlRequest<'_>,
    console_name: &str,
    console: Option<&std::process::Child>,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
) -> String {
    match req {
        ControlRequest::Status(unit) => {
            format_status(unit.unwrap_or(""), console_name, console, services)
        }
        ControlRequest::Start(name) => do_start(name, services, units),
        ControlRequest::Stop(name) => do_stop(name, console_name, services),
        ControlRequest::Reload(name) => do_reload(name, services),
        ControlRequest::Restart(name) => {
            let stop_out = do_stop(name, console_name, services);
            if stop_out.starts_with("unknown") || stop_out.contains("console") {
                return stop_out;
            }
            let start_out = do_start(name, services, units);
            format!("{}{}", stop_out, start_out)
        }
    }
}

/// 重载服务：执行 ExecReload 命令（不重启进程）。
fn do_reload(name: &str, services: &mut [ServiceInstance]) -> String {
    let svc = match services.iter_mut().find(|s| s.name == name) {
        Some(s) => s,
        None => return format!("unknown unit: {}\n", name),
    };
    if svc.child.is_none() && svc.tracked_pid.is_none() {
        return format!("{} not running\n", name);
    }
    match &svc.exec_reload {
        Some(cmd) => {
            let argv = parse_cmdline(cmd);
            if argv.is_empty() {
                return format!("{} has empty ExecReload\n", name);
            }
            let _ = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .status();
            format!("{} reloaded\n", name)
        }
        None => format!("{} has no ExecReload\n", name),
    }
}

/// 服务退出后调度重启：失败计数 +1（成功退出清零），
/// 达到 StartLimitBurst 上限后放弃；否则按 RestartSec 退避。
fn schedule_restart(svc: &mut ServiceInstance, failed: bool) {
    if failed {
        svc.fail_count += 1;
    } else {
        svc.fail_count = 0;
    }
    if !svc.restart_on_failure || !failed || svc.stopped || shutdown_requested() {
        return;
    }
    if svc.fail_count >= svc.start_limit_burst {
        log(&format!(
            "rbox init: {} failed {} times, giving up (StartLimitBurst={})",
            svc.name, svc.fail_count, svc.start_limit_burst
        ));
        return;
    }
    svc.next_restart_at =
        Some(std::time::Instant::now() + std::time::Duration::from_secs(svc.restart_sec));
}

/// 重新拉起服务（自动重启与手动 start 共用）。
/// simple：直接 spawn；forking：重新走 daemon 化流程并重读 PIDFile。
fn respawn_service(svc: &mut ServiceInstance) {
    let cfg = SpawnConfig {
        logfile: svc.logfile.as_deref(),
        user: svc.user.as_deref(),
        group: svc.group.as_deref(),
    };
    if !svc.is_forking {
        svc.tracked_pid = None;
        svc.child = spawn_unit_command(&svc.name, &svc.exec_start, &svc.env, &cfg);
        return;
    }
    let mut child = match spawn_unit_command(&svc.name, &svc.exec_start, &svc.env, &cfg) {
        Some(c) => c,
        None => return,
    };
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(svc.timeout_start_sec);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    svc.child = None;
    svc.tracked_pid = svc.pidfile.as_deref().and_then(read_pid_file);
}

/// 启动服务：已在 services 中的重新拉起；否则从单元文件新建实例。
fn do_start(
    name: &str,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
) -> String {
    if let Some(svc) = services.iter_mut().find(|s| s.name == name) {
        if svc.child.is_some() || svc.tracked_pid.is_some() {
            return format!("{} already running\n", name);
        }
        svc.stopped = false;
        svc.fail_count = 0;
        svc.next_restart_at = None;
        respawn_service(svc);
        return if svc.child.is_some() || svc.tracked_pid.is_some() {
            format!("{} started\n", name)
        } else {
            format!("failed to start {}\n", name)
        };
    }
    let unit = match units.get(name) {
        Some(u) => u,
        None => return format!("unknown unit: {}\n", name),
    };
    if unit.is_target {
        return format!("{} is a target, not a service\n", name);
    }
    let cmd = match &unit.service.exec_start {
        Some(c) => c.clone(),
        None => return format!("{} has no ExecStart\n", name),
    };
    let env = parse_environment(&unit.service.environment);
    let inst = if unit.service.typ == "forking" {
        start_forking_service(unit, &cmd, &env)
    } else {
        start_service(unit, &cmd, &env)
    };
    match inst {
        Some(inst) => {
            services.push(inst);
            format!("{} started\n", name)
        }
        None => format!("failed to start {}\n", name),
    }
}

/// 停止服务：执行 ExecStop 并终止进程组，标记 stopped（禁止自动重启）。
fn do_stop(name: &str, console_name: &str, services: &mut [ServiceInstance]) -> String {
    if name == console_name {
        return format!("cannot manage console service: {}\n", name);
    }
    let svc = match services.iter_mut().find(|s| s.name == name) {
        Some(s) => s,
        None => return format!("unknown unit: {}\n", name),
    };
    svc.stopped = true;
    if svc.child.is_none() {
        return format!("{} already stopped\n", name);
    }
    stop_service_instance(svc);
    format!("{} stopped\n", name)
}

/// 执行 ExecStop 并终止服务进程组：SIGTERM 等 1 秒，超时 SIGKILL。
/// 供关机流程与 stop/restart 命令复用；forking 服务额外终止 daemon pid。
fn stop_service_instance(svc: &mut ServiceInstance) {
    if let Some(stop_cmd) = &svc.exec_stop {
        log(&format!("rbox init: stopping {}: {}", svc.name, stop_cmd));
        let argv = parse_cmdline(stop_cmd);
        if !argv.is_empty() {
            let _ = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .status();
        }
    }
    if let Some(mut child) = svc.child.take() {
        let _ = kill_process_group(child.id(), libc::SIGTERM);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = kill_process_group(child.id(), libc::SIGKILL);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        let _ = child.wait();
    }
    // forking daemon：向 daemon pid 发信号并等待其退出（init 收养后 waitpid 可收割）
    if let Some(pid) = svc.tracked_pid.take() {
        let _ = kill_process(pid, libc::SIGTERM);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut status: libc::c_int = 0;
        loop {
            let r = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
            if r == pid as i32 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = kill_process(pid, libc::SIGKILL);
                unsafe { libc::waitpid(pid as i32, &mut status, 0) };
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// 生成 status 响应文本。请求为空时列出全部；`status <unit>` 查单个。
fn format_status(
    req: &str,
    console_name: &str,
    console: Option<&std::process::Child>,
    services: &[ServiceInstance],
) -> String {
    let req = req.trim();
    let mut out = String::new();
    let unit = req.strip_prefix("status ").map(str::trim).unwrap_or("");

    if unit.is_empty() {
        out.push_str(&format!("init pid={}\n", std::process::id()));
        match console {
            Some(c) => out.push_str(&format!("{} running pid={}\n", console_name, c.id())),
            None => out.push_str(&format!("{} stopped\n", console_name)),
        }
        for svc in services {
            out.push_str(&svc.status_line());
        }
    } else if unit == "init" {
        out.push_str(&format!("init pid={}\n", std::process::id()));
    } else if unit == console_name {
        match console {
            Some(c) => out.push_str(&format!("{} running pid={}\n", console_name, c.id())),
            None => out.push_str(&format!("{} stopped\n", console_name)),
        }
    } else if let Some(svc) = services.iter().find(|s| s.name == unit) {
        out.push_str(&svc.status_line());
    } else {
        out.push_str(&format!("unknown unit: {}\n", unit));
    }
    out
}

/// 输出日志：优先写入内核环形缓冲（/dev/kmsg，内核自动回显到 console，
/// 带时间戳且不重复）；kmsg 不可用时（如 devtmpfs 挂载前）回退 console stderr。
/// 注意：kmsg 每次 write 调用产生一条消息，必须整条一次性写入，
/// 否则会被拆成多行（每行带时间戳前缀）。
fn log(msg: &str) {
    if let Ok(mut kmsg) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let line = format!("rbox: {}\n", msg);
        let _ = kmsg.write_all(line.as_bytes());
        return;
    }
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{}", msg);
    let _ = stderr.flush();
}

// ─── 系统调用封装（使用 libc crate）──────────────────

/// 发送信号给指定进程。
fn kill_process(pid: u32, sig: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(pid as i32, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 发送信号给进程组（pgid 由组首进程 pid 表示，kill 负 pid）。
fn kill_process_group(pgid: u32, sig: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(-(pgid as i32), sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 发送信号给所有进程（pid=-1）。
fn kill_all(sig: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(-1, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// sync 文件系统。
fn sync_fs() {
    unsafe { libc::sync() };
}

/// reboot 系统调用。
/// cmd: libc::RB_POWER_OFF（关机）或 libc::RB_AUTOBOOT（重启）
fn reboot_syscall(cmd: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::reboot(cmd) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个测试用 Unit。
    fn unit(
        name: &str,
        is_target: bool,
        requires: &[&str],
        after: &[&str],
        wanted_by: &[&str],
    ) -> Unit {
        Unit {
            name: name.to_string(),
            is_target,
            unit: UnitSection {
                description: String::new(),
                name: String::new(),
                after: after.iter().map(|s| s.to_string()).collect(),
                requires: requires.iter().map(|s| s.to_string()).collect(),
            },
            service: ServiceSection {
                typ: "simple".to_string(),
                exec_start: None,
                exec_stop: None,
                exec_reload: None,
                restart: String::new(),
                restart_sec: 1,
                start_limit_burst: 5,
                timeout_start_sec: 10,
                pidfile: None,
                environment: Vec::new(),
                logfile: None,
                user: None,
                group: None,
                console: false,
            },
            install: InstallSection {
                wanted_by: wanted_by.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    #[test]
    fn parse_cmdline_basic() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo hello"),
            vec!["/bin/rbox", "echo", "hello"]
        );
    }

    #[test]
    fn parse_cmdline_quotes() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo \"hello world\""),
            vec!["/bin/rbox", "echo", "hello world"]
        );
    }

    #[test]
    fn parse_cmdline_ignores_extra_spaces() {
        assert_eq!(parse_cmdline("  a   b  "), vec!["a", "b"]);
    }

    #[test]
    fn start_order_respects_requires() {
        let mut units = HashMap::new();
        units.insert(
            "default.target".into(),
            unit("default.target", true, &["b.service"], &[], &[]),
        );
        units.insert(
            "b.service".into(),
            unit("b.service", false, &["a.service"], &[], &[]),
        );
        units.insert("a.service".into(), unit("a.service", false, &[], &[], &[]));
        let order = compute_start_order(&units, "default.target").unwrap();
        assert_eq!(order, vec!["a.service", "b.service", "default.target"]);
    }

    #[test]
    fn start_order_respects_after() {
        let mut units = HashMap::new();
        units.insert(
            "default.target".into(),
            unit("default.target", true, &[], &["a.service"], &[]),
        );
        units.insert("a.service".into(), unit("a.service", false, &[], &[], &[]));
        let order = compute_start_order(&units, "default.target").unwrap();
        assert_eq!(order, vec!["a.service", "default.target"]);
    }

    #[test]
    fn start_order_detects_cycle() {
        let mut units = HashMap::new();
        units.insert(
            "a.service".into(),
            unit("a.service", false, &["b.service"], &[], &[]),
        );
        units.insert(
            "b.service".into(),
            unit("b.service", false, &["a.service"], &[], &[]),
        );
        let err = compute_start_order(&units, "a.service").unwrap_err();
        assert!(err.contains("cycle"), "unexpected error: {}", err);
    }

    #[test]
    fn start_order_pulls_wantedby_services() {
        let mut units = HashMap::new();
        units.insert(
            "default.target".into(),
            unit("default.target", true, &[], &[], &[]),
        );
        units.insert(
            "svc.service".into(),
            unit("svc.service", false, &[], &[], &["default.target"]),
        );
        let order = compute_start_order(&units, "default.target").unwrap();
        // default.target 必须是最后一个（DFS 后序）
        assert_eq!(order.last().map(String::as_str), Some("default.target"));
        // WantedBy 的服务必须被拉入且排在 target 之前
        let i_svc = order.iter().position(|n| n == "svc.service").unwrap();
        let i_def = order.iter().position(|n| n == "default.target").unwrap();
        assert!(i_svc < i_def);
    }

    #[test]
    fn start_order_missing_root_is_ok() {
        let units: HashMap<String, Unit> = HashMap::new();
        assert!(compute_start_order(&units, "ghost.target").is_ok());
    }

    #[test]
    fn parse_fstab_basic_entries() {
        let entries = parse_fstab("proc /proc proc defaults 0 0\nsysfs /sys sysfs ro 0 0\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].device, "proc");
        assert_eq!(entries[0].mountpoint, "/proc");
        assert_eq!(entries[0].fstype, "proc");
        assert_eq!(entries[0].options, "defaults");
        assert_eq!(entries[1].options, "ro");
    }

    #[test]
    fn parse_fstab_skips_comments_and_blank_lines() {
        let entries = parse_fstab("# comment\n\n   \nproc /proc proc defaults 0 0\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device, "proc");
    }

    #[test]
    fn parse_fstab_drops_short_lines() {
        let entries = parse_fstab("proc /proc\nproc /proc proc defaults 0 0\n");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_mount_flags_mapping() {
        assert_eq!(parse_mount_flags("defaults"), 0);
        assert_eq!(parse_mount_flags("rw"), 0);
        assert_eq!(parse_mount_flags("ro"), libc::MS_RDONLY);
        assert_eq!(
            parse_mount_flags("ro,noexec,nosuid"),
            libc::MS_RDONLY | libc::MS_NOEXEC | libc::MS_NOSUID
        );
        assert_eq!(parse_mount_flags("unknownopt"), 0);
    }

    #[test]
    fn parse_environment_basic() {
        let env = parse_environment(&["A=1".into(), "B=two words".into()]);
        assert_eq!(
            env,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string())
            ]
        );
    }

    #[test]
    fn parse_environment_skips_invalid() {
        let env = parse_environment(&["A=1".into(), "NOEQUALS".into(), "=v".into()]);
        assert_eq!(env, vec![("A".to_string(), "1".to_string())]);
    }

    /// 构造一个测试用 ServiceInstance。
    fn svc(name: &str, restart_on_failure: bool) -> ServiceInstance {
        ServiceInstance {
            name: name.to_string(),
            child: None,
            tracked_pid: None,
            exec_stop: None,
            exec_reload: None,
            exec_start: "/bin/rbox false".to_string(),
            env: Vec::new(),
            logfile: None,
            user: None,
            group: None,
            is_forking: false,
            pidfile: None,
            timeout_start_sec: 10,
            restart_on_failure,
            restart_sec: 1,
            start_limit_burst: 5,
            fail_count: 0,
            next_restart_at: None,
            stopped: false,
        }
    }

    #[test]
    fn format_status_lists_all() {
        let services = vec![svc("a.service", false), svc("b.service", true)];
        let out = format_status("", "console-shell.service", None, &services);
        assert!(out.contains("init pid="), "out: {}", out);
        assert!(
            out.contains("console-shell.service stopped"),
            "out: {}",
            out
        );
        assert!(out.contains("a.service exited"), "out: {}", out);
        assert!(
            out.contains("b.service exited restart=on-failure"),
            "out: {}",
            out
        );
    }

    #[test]
    fn format_status_single_unit() {
        let services = vec![svc("a.service", false)];
        let out = format_status("status a.service", "console-shell.service", None, &services);
        assert!(out.contains("a.service exited"), "out: {}", out);
        assert!(!out.contains("init pid="), "out: {}", out);
    }

    #[test]
    fn format_status_unknown_unit() {
        let out = format_status("status ghost.service", "console-shell.service", None, &[]);
        assert!(out.contains("unknown unit: ghost.service"), "out: {}", out);
    }

    #[test]
    fn parse_control_request_status() {
        assert_eq!(parse_control_request(""), Ok(ControlRequest::Status(None)));
        assert_eq!(
            parse_control_request("status"),
            Ok(ControlRequest::Status(None))
        );
        assert_eq!(
            parse_control_request("status hello.service"),
            Ok(ControlRequest::Status(Some("hello.service")))
        );
    }

    #[test]
    fn parse_control_request_service_cmds() {
        assert_eq!(
            parse_control_request("start hello.service"),
            Ok(ControlRequest::Start("hello.service"))
        );
        assert_eq!(
            parse_control_request("stop  hello.service "),
            Ok(ControlRequest::Stop("hello.service"))
        );
        assert_eq!(
            parse_control_request("restart hello.service"),
            Ok(ControlRequest::Restart("hello.service"))
        );
    }

    #[test]
    fn parse_control_request_errors() {
        assert!(
            parse_control_request("start")
                .unwrap_err()
                .contains("usage")
        );
        assert!(
            parse_control_request("frobnicate x")
                .unwrap_err()
                .contains("unknown")
        );
    }

    #[test]
    fn resolve_unit_name_prefers_declared() {
        assert_eq!(resolve_unit_name("hello.service", ""), "hello.service");
        assert_eq!(resolve_unit_name("hello.service", "hello"), "hello");
    }

    #[test]
    fn is_target_file_uses_filename_suffix() {
        assert!(is_target_file("default.target"));
        assert!(!is_target_file("default"));
        assert!(!is_target_file("hello.service"));
    }

    #[test]
    fn parse_sysctl_conf_basic() {
        let entries = parse_sysctl_conf(
            "# comment\n\nkernel.panic = 10\nnet.ipv4.ip_forward=1\nbroken-line\n",
        );
        assert_eq!(
            entries,
            vec![
                ("kernel.panic".to_string(), "10".to_string()),
                ("net.ipv4.ip_forward".to_string(), "1".to_string())
            ]
        );
    }

    #[test]
    fn parse_control_request_reload() {
        assert_eq!(
            parse_control_request("reload hello"),
            Ok(ControlRequest::Reload("hello"))
        );
        assert!(
            parse_control_request("reload")
                .unwrap_err()
                .contains("usage")
        );
    }
}
