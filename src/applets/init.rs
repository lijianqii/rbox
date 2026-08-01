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
use std::io::Write;
use std::path::Path;
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
fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst) || REBOOT_REQUESTED.load(Ordering::SeqCst)
}

pub struct Init;
pub static INIT: &Init = &Init;

const SYSTEM_DIR: &str = "/etc/rbox/system";
const DEFAULT_TARGET: &str = "default.target";

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
    exec_stop: Option<String>,
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

        // 1. 基本文件系统初始化
        mount_basic_fs();
        log("rbox init: basic filesystems mounted");

        // 2. 解析所有单元文件
        let units = match load_all_units() {
            Ok(u) => {
                log(&format!("rbox init: loaded {} unit(s)", u.len()));
                u
            }
            Err(e) => {
                log(&format!("rbox init: failed to load units: {}", e));
                return reap_with_shutdown(None, &mut Vec::new());
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
                return reap_with_shutdown(None, &mut Vec::new());
            }
        };

        // 4. 依次启动服务，记录已启动的实例
        let mut services: Vec<ServiceInstance> = Vec::new();
        let mut console_child: Option<std::process::Child> = None;
        for unit_name in &order {
            if let Some(unit) = units.get(unit_name) {
                if unit.is_target {
                    log(&format!("rbox init: reached target {}", unit_name));
                    continue;
                }
                if let Some(cmd) = &unit.service.exec_start {
                    if !unit.service.typ.is_empty() && unit.service.typ != "simple" {
                        log(&format!(
                            "rbox init: warning: {} Type={:?} unsupported, treating as simple",
                            unit_name, unit.service.typ
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
                    let is_console = unit_name == "console-shell.service"
                        || cmd.contains("shell");
                    if is_console {
                        console_child = spawn_unit_command(unit, cmd);
                    } else if let Some(inst) = start_service(unit, cmd) {
                        services.push(inst);
                    }
                }
            }
        }

        log("rbox init: startup complete");

        // 5. 主循环：回收子进程，等待关机标志
        reap_with_shutdown(console_child, &mut services)
    }
}

/// 安装 SIGTERM/SIGINT 信号处理器。
fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, signal_handler as *const () as usize);
        libc::signal(libc::SIGINT, signal_handler as *const () as usize);
    }
}

/// 主循环：管理 console shell（退出则 respawn）、回收服务进程，检测关机标志。
fn reap_with_shutdown(
    mut console: Option<std::process::Child>,
    services: &mut [ServiceInstance],
) -> ExitCode {
    loop {
        // 1. console shell：运行中则检查退出，退出后标记待 respawn
        if let Some(child) = console.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
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

        // 2. 回收已退出的服务进程，避免僵尸
        for svc in services.iter_mut() {
            if let Some(child) = svc.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    log(&format!(
                        "rbox init: service {} exited (code {:?})",
                        svc.name,
                        status.code()
                    ));
                    svc.child = None;
                }
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

/// 执行有序关机：逆序执行 ExecStop，终止服务进程，杀残留进程，再 power off。
fn do_shutdown(services: &mut [ServiceInstance]) -> ExitCode {
    log("rbox init: shutting down");
    for svc in services.iter_mut().rev() {
        if let Some(stop_cmd) = &svc.exec_stop {
            log(&format!("rbox init: stopping {}: {}", svc.name, stop_cmd));
            let argv = parse_cmdline(stop_cmd);
            if !argv.is_empty() {
                let _ = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .status();
            }
        }
        // 优雅终止：SIGTERM 等 1 秒，超时 SIGKILL
        if let Some(mut child) = svc.child.take() {
            let _ = kill_process(child.id(), libc::SIGTERM);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            let _ = child.kill();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
            let _ = child.wait();
        }
    }
    log("rbox init: sending SIGTERM to all processes");
    let _ = kill_all(15);
    std::thread::sleep(std::time::Duration::from_millis(500));
    sync_fs();
    let is_reboot = REBOOT_REQUESTED.load(Ordering::SeqCst);
    if is_reboot {
        log("rbox init: rebooting");
    } else {
        log("rbox init: power off");
    }
    let _ = reboot_syscall(if is_reboot { libc::RB_AUTOBOOT } else { libc::RB_POWER_OFF });
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

/// 解析 ExecStart 命令并 spawn 子进程，返回子进程句柄。
/// ExecStart 以 `rbox <applet>` 开头时走 /bin/rbox。
fn spawn_unit_command(unit: &Unit, cmd: &str) -> Option<std::process::Child> {
    let argv = parse_cmdline(cmd);
    if argv.is_empty() {
        return None;
    }
    let (program, args) = if argv[0] == "rbox" && argv.len() >= 2 {
        ("/bin/rbox", &argv[1..])
    } else {
        (argv[0].as_str(), &argv[1..])
    };
    match std::process::Command::new(program).args(args).spawn() {
        Ok(child) => {
            log(&format!(
                "rbox init: started {} (pid {})",
                unit.name,
                child.id()
            ));
            Some(child)
        }
        Err(e) => {
            log(&format!("rbox init: failed to start {}: {}", unit.name, e));
            None
        }
    }
}



/// 挂载 proc / sys / devtmpfs 等基本文件系统（若已挂载则跳过）。
fn mount_basic_fs() {
    let _ = fs::create_dir_all("/proc");
    let _ = run_mount("proc", "/proc", "proc");
    let _ = fs::create_dir_all("/sys");
    let _ = run_mount("sysfs", "/sys", "sysfs");
    let _ = fs::create_dir_all("/dev");
    let _ = run_mount("devtmpfs", "/dev", "devtmpfs");
    let _ = fs::create_dir_all("/dev/pts");
    let _ = run_mount("devpts", "/dev/pts", "devpts");
    let _ = fs::create_dir_all("/tmp");
    // 为 shell 提供默认 PATH
    if std::env::var_os("PATH").is_none() {
        // SAFETY: init 是单线程 PID 1，无并发修改环境变量风险
        unsafe { std::env::set_var("PATH", "/bin:/sbin:/usr/bin:/usr/sbin") };
    }
}

fn run_mount(src: &str, tgt: &str, fstype: &str) -> std::io::Result<()> {
    use std::ffi::CString;
    let s = CString::new(src).unwrap();
    let t = CString::new(tgt).unwrap();
    let f = CString::new(fstype).unwrap();
    let rc = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            f.as_ptr(),
            0,
            std::ptr::null::<std::ffi::c_void>(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
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
                        // 文件名去掉 .toml 作为单元名
                        unit.name = path
                            .file_stem()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        unit.is_target = unit.name.ends_with(".target");
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
fn compute_start_order(
    units: &HashMap<String, Unit>,
    root: &str,
) -> Result<Vec<String>, String> {
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

/// 启动一个服务（simple 类型），返回运行时实例。
fn start_service(unit: &Unit, cmd: &str) -> Option<ServiceInstance> {
    let child = spawn_unit_command(unit, cmd)?;
    Some(ServiceInstance {
        name: unit.name.clone(),
        child: Some(child),
        exec_stop: unit.service.exec_stop.clone(),
    })
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

/// 往 console 输出日志。
fn log(msg: &str) {
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
                after: after.iter().map(|s| s.to_string()).collect(),
                requires: requires.iter().map(|s| s.to_string()).collect(),
            },
            service: ServiceSection {
                typ: "simple".to_string(),
                exec_start: None,
                exec_stop: None,
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
        units.insert("b.service".into(), unit("b.service", false, &["a.service"], &[], &[]));
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
        units.insert("a.service".into(), unit("a.service", false, &["b.service"], &[], &[]));
        units.insert("b.service".into(), unit("b.service", false, &["a.service"], &[], &[]));
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
}
