//! init 控制协议服务端：监听 unix socket，处理 status/start/stop/restart/reload。

use crate::applets::core::control::status_socket;
use crate::applets::core::init::services::{
    EXEC_COMMAND_TIMEOUT, ServiceInstance, respawn_service, run_command_with_timeout,
    start_forking_service, start_service, stop_service_instance, unit_environment,
};
use crate::applets::core::init::units::{Unit, parse_cmdline};
use crate::applets::core::log;
use crate::applets::proc::{ProcMem, collect_processes};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};

/// 创建控制 socket（非阻塞）；失败时返回 None（不影响启动）。
pub(crate) fn create_status_listener() -> Option<UnixListener> {
    // UnixListener::bind 不会清理已存在的 socket 文件；initramfs 重启时 /run 是
    // tmpfs 会自然消失，但持久盘启动（fstab 未挂 tmpfs /run）或 init 被 re-exec 时
    // 残留文件会导致 bind EADDRINUSE，控制协议静默失效。这里先尝试移除旧文件。
    let _ = std::fs::remove_file(status_socket());
    match UnixListener::bind(status_socket()) {
        Ok(l) => {
            // 限制权限：仅 root 可连（服务管理接口，防普通用户 stop/restart 任意服务）
            let _ =
                std::fs::set_permissions(status_socket(), std::fs::Permissions::from_mode(0o600));
            let _ = l.set_nonblocking(true);
            Some(l)
        }
        Err(e) => {
            log(&format!("rbox init: status socket failed: {}", e));
            None
        }
    }
}

/// 处理一次控制连接：读一行请求，分发到 status/start/stop/restart/reload，回写响应，关闭。
/// 在独立线程中执行（由主循环 spawn），共享服务状态通过 Mutex；
/// 连接设为非阻塞并用 10ms poll 等数据，避免异常客户端（连接后不发数据）挂住线程。
pub(crate) fn handle_control_connection(
    mut stream: UnixStream,
    services: std::sync::Arc<std::sync::Mutex<Vec<ServiceInstance>>>,
    units: std::sync::Arc<std::sync::Mutex<HashMap<String, Unit>>>,
) {
    let _ = stream.set_nonblocking(true);
    let mut req = String::new();
    // 等数据：最多 10ms（主循环单次被阻塞量级可接受）；非阻塞读直到 EAGAIN 或换行
    let mut buf = [0u8; 256];
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(10);
    loop {
        if req.ends_with('\n') || std::time::Instant::now() >= deadline {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                req.push_str(&String::from_utf8_lossy(&buf[..n]));
                if req.len() > 4096 {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
    // daemon-reload：可变锁重载单元目录（新增/修改/删除的单元立即生效）
    if req.trim() == "daemon-reload" {
        let resp = match units.lock() {
            Ok(mut um) => match crate::applets::core::init::units::load_all_units() {
                Ok(new_units) => {
                    let n = new_units.len();
                    *um = new_units;
                    format!("reloaded {} units\n", n)
                }
                Err(e) => format!("reload failed: {}\n", e),
            },
            Err(_) => "error: unit state poisoned\n".to_string(),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }
    let resp = match parse_control_request(&req) {
        Ok(r @ ControlRequest::Status(_)) => {
            // status 需要 CPU 占用率：双采样 /proc（间隔 300ms）。
            // 采样在锁外做，不阻塞服务管理；生成文本时再短暂持锁。
            let prev = collect_processes();
            std::thread::sleep(std::time::Duration::from_millis(PROC_SAMPLE_MS));
            let now = collect_processes();
            let interval = PROC_SAMPLE_MS as f64 / 1000.0;
            match (services.lock(), units.lock()) {
                (Ok(mut guard), Ok(um)) => {
                    execute_control_request(r, &mut guard, &um, Some((&prev, &now, interval)))
                }
                _ => "error: service state poisoned\n".to_string(),
            }
        }
        Ok(r) => match (services.lock(), units.lock()) {
            (Ok(mut guard), Ok(um)) => execute_control_request(r, &mut guard, &um, None),
            _ => "error: service state poisoned\n".to_string(),
        },
        Err(e) => format!("error: {}\n", e),
    };
    // 读阶段用非阻塞避免挂住线程；写响应时恢复阻塞，避免小响应遇 WouldBlock 丢失
    let _ = stream.set_nonblocking(false);
    let _ = stream.write_all(resp.as_bytes());
}

/// status 的 CPU 占用率双采样间隔（毫秒）。
const PROC_SAMPLE_MS: u64 = 300;

/// 控制请求：status 查询或服务管理命令。
#[derive(Debug, PartialEq)]
pub(crate) enum ControlRequest<'a> {
    /// status [unit]：unit 为 None 时列出全部
    Status(Option<&'a str>),
    Start(&'a str),
    Stop(&'a str),
    Restart(&'a str),
    Reload(&'a str),
    ResetFailed(Option<&'a str>),
    Enable(&'a str),
    Disable(&'a str),
    IsEnabled(&'a str),
    Isolate(&'a str),
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
        "reset-failed" => Ok(ControlRequest::ResetFailed(arg)),
        "enable" | "disable" | "is-enabled" | "isolate" => match arg {
            Some(unit) => Ok(match cmd {
                "enable" => ControlRequest::Enable(unit),
                "disable" => ControlRequest::Disable(unit),
                "isolate" => ControlRequest::Isolate(unit),
                _ => ControlRequest::IsEnabled(unit),
            }),
            None => Err(format!("usage: {} <unit>", cmd)),
        },
        other => Err(format!("unknown command: {}", other)),
    }
}

/// 执行控制请求，返回响应文本。
/// `procs` 为 status 双采样快照（prev, now, 采样间隔秒），用于 CPU 占用率；
/// 非 status 请求传 None。
fn execute_control_request(
    req: ControlRequest<'_>,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
    procs: Option<(&[ProcMem], &[ProcMem], f64)>,
) -> String {
    match req {
        ControlRequest::Status(unit) => format_status(unit.unwrap_or(""), services, units, procs),
        ControlRequest::Start(name) => do_start(name, services, units),
        ControlRequest::Stop(name) => do_stop(name, services),
        ControlRequest::Reload(name) => do_reload(name, services),
        ControlRequest::Enable(name) => do_set_enabled(name, true),
        ControlRequest::Disable(name) => do_set_enabled(name, false),
        ControlRequest::IsEnabled(name) => do_is_enabled(name),
        ControlRequest::Isolate(name) => do_isolate(name, services, units),
        ControlRequest::ResetFailed(name) => {
            let mut out = String::new();
            for svc in services.iter_mut() {
                if name.is_none_or(|n| n == svc.name) {
                    svc.fail_count = 0;
                    svc.first_failure_at = None;
                    svc.next_restart_at = None;
                    out.push_str(&format!("{} reset-failed\n", svc.name));
                }
            }
            if out.is_empty() {
                out = "no matching units\n".to_string();
            }
            out
        }
        ControlRequest::Restart(name) => {
            let stop_out = do_stop(name, services);
            if stop_out.starts_with("unknown") {
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
        Some(cmd) => run_reload_cmd(name, cmd),
        None => format!("{} has no ExecReload\n", name),
    }
}

/// 执行 ExecReload 命令并返回响应。
fn run_reload_cmd(name: &str, cmd: &str) -> String {
    let argv = parse_cmdline(cmd);
    if argv.is_empty() {
        return format!("{} has empty ExecReload\n", name);
    }
    if !run_command_with_timeout(&argv, EXEC_COMMAND_TIMEOUT) {
        return format!("{} reload command timed out\n", name);
    }
    format!("{} reloaded\n", name)
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
        svc.first_failure_at = None;
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
    // 条件不满足：拒绝启动（与启动流程一致，isolate 等批量启动不会绕过条件）
    if !crate::applets::core::init::conditions_met(unit) {
        return format!("{} skipped (condition not met)\n", name);
    }
    // Requisite= 未激活：拒绝启动
    for r in &unit.unit.requisite {
        let active = services
            .iter()
            .any(|s| &s.name == r && (s.child.is_some() || s.tracked_pid.is_some() || s.active));
        if !active {
            return format!("{} skipped (requisite {} not active)\n", name, r);
        }
    }
    let cmd = match &unit.service.exec_start {
        Some(c) => c.clone(),
        None => return format!("{} has no ExecStart\n", name),
    };
    // Conflicts=：启动前停止互斥的已运行单元（含反向：其他单元声明与本单元冲突）
    let mut conflict_out = String::new();
    let mut conflicting: Vec<String> = unit.unit.conflicts.clone();
    for (other_name, other) in units.iter() {
        if other_name != name && other.unit.conflicts.iter().any(|c| c == name) {
            conflicting.push(other_name.clone());
        }
    }
    for cname in conflicting {
        if services
            .iter()
            .any(|s| s.name == cname && (s.child.is_some() || s.tracked_pid.is_some()))
        {
            conflict_out.push_str(&do_stop(&cname, services));
        }
    }
    let env = unit_environment(unit);
    let inst = if unit.service.typ == "forking" {
        start_forking_service(unit, &cmd, &env)
    } else {
        start_service(unit, &cmd, &env)
    };
    match inst {
        Some(inst) => {
            services.push(inst);
            format!("{}{} started\n", conflict_out, name)
        }
        None => format!("{}failed to start {}\n", conflict_out, name),
    }
}

/// enable/disable：重命名单元文件为 `*.toml.disabled`（或恢复）。
fn do_set_enabled(name: &str, enable: bool) -> String {
    use crate::applets::core::init::units::scan_unit_files;
    for (uname, path, enabled) in scan_unit_files() {
        if uname != name {
            continue;
        }
        if enabled == enable {
            return format!(
                "{} is already {}\n",
                name,
                if enable { "enabled" } else { "disabled" }
            );
        }
        let new_path = if enable {
            // x.toml.disabled -> x.toml
            let fname = path.file_name().unwrap_or_default().to_string_lossy();
            path.with_file_name(fname.trim_end_matches(".disabled"))
        } else {
            path.with_file_name(format!(
                "{}.disabled",
                path.file_name().unwrap_or_default().to_string_lossy()
            ))
        };
        return match std::fs::rename(&path, &new_path) {
            Ok(()) => format!("{} {}\n", name, if enable { "enabled" } else { "disabled" }),
            Err(e) => format!("failed to update {}: {}\n", name, e),
        };
    }
    format!("unknown unit: {}\n", name)
}

/// is-enabled：查询单元文件是否启用。
fn do_is_enabled(name: &str) -> String {
    use crate::applets::core::init::units::scan_unit_files;
    for (uname, _, enabled) in scan_unit_files() {
        if uname == name {
            return format!("{}\n", if enabled { "enabled" } else { "disabled" });
        }
    }
    "not-found\n".to_string()
}

/// isolate：切换到指定 target——停止不在其依赖闭包内的运行服务，启动闭包内未启动的单元。
fn do_isolate(
    target: &str,
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
) -> String {
    let Some(t) = units.get(target) else {
        return format!("unknown unit: {}\n", target);
    };
    if !t.is_target {
        return format!("{} is not a target\n", target);
    }
    // 依赖闭包（Requires/Wants 传递）
    let mut closure: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack = vec![target.to_string()];
    while let Some(n) = stack.pop() {
        if !closure.insert(n.clone()) {
            continue;
        }
        if let Some(u) = units.get(&n) {
            for d in u.unit.requires.iter().chain(u.unit.wants.iter()) {
                stack.push(d.clone());
            }
        }
        // 反向 WantedBy：WantedBy 含 n 的单元也属于该 target（与启动逻辑一致），
        // 否则 isolate 会误停 console-shell 等由 target 拉起的单元
        for (oname, ou) in units.iter() {
            if ou.install.wanted_by.iter().any(|w| w == &n) {
                stack.push(oname.clone());
            }
        }
    }
    let mut out = String::new();
    // 停止闭包外的运行服务
    let running: Vec<String> = services
        .iter()
        .filter(|s| {
            (s.child.is_some() || s.tracked_pid.is_some() || s.active) && !closure.contains(&s.name)
        })
        .map(|s| s.name.clone())
        .collect();
    for n in running {
        out.push_str(&do_stop_one(&n, services));
    }
    // 启动闭包内未运行的单元（target 本身无进程）
    let to_start: Vec<String> = closure
        .iter()
        .filter(|n| {
            units
                .get(*n)
                .is_some_and(|u| !u.is_target && u.service.exec_start.is_some())
                && !services.iter().any(|s| {
                    &s.name == *n && (s.child.is_some() || s.tracked_pid.is_some() || s.active)
                })
        })
        .cloned()
        .collect();
    for n in to_start {
        out.push_str(&do_start(&n, services, units));
    }
    out.push_str(&format!("isolated to {}\n", target));
    out
}

/// 停止服务：执行 ExecStop 并终止进程组，标记 stopped（禁止自动重启）。
/// PartOf= 联动的单元（其 part_of 含 name）一并停止。
fn do_stop(name: &str, services: &mut [ServiceInstance]) -> String {
    let mut out = do_stop_one(name, services);
    let linked: Vec<String> = services
        .iter()
        .filter(|s| {
            s.name != name
                && s.unit.unit.part_of.iter().any(|p| p == name)
                && (s.child.is_some() || s.tracked_pid.is_some() || s.active)
        })
        .map(|s| s.name.clone())
        .collect();
    for l in linked {
        out.push_str(&do_stop_one(&l, services));
    }
    out
}

fn do_stop_one(name: &str, services: &mut [ServiceInstance]) -> String {
    let svc = match services.iter_mut().find(|s| s.name == name) {
        Some(s) => s,
        None => return format!("unknown unit: {}\n", name),
    };
    svc.stopped = true;
    if svc.child.is_none() && svc.tracked_pid.is_none() && !svc.active {
        return format!("{} already stopped\n", name);
    }
    stop_service_instance(svc);
    format!("{} stopped\n", name)
}

/// 生成 status 响应文本。`unit` 为空列出全部（含未启动单元）；否则查单个单元。
/// 未启动单元显示 not-started，与运行实例（services）状态合并呈现。
/// 有 procs 快照时：init 在最顶（带自身 cpu/mem），各服务以 tree 风格
/// （├──/└──/│）分支，运行实例挂进程树明细（pid/状态/CPU%/内存）。
fn format_status(
    unit: &str,
    services: &[ServiceInstance],
    units: &HashMap<String, Unit>,
    procs: Option<(&[ProcMem], &[ProcMem], f64)>,
) -> String {
    let unit = unit.trim();
    let mut out = String::new();
    // 快照索引（有采样时）；无采样（单测/异常）时 prev/now 为 None
    let (prev_map, now_map, interval) = match procs {
        Some((prev, now, iv)) => {
            let pm: HashMap<u32, &ProcMem> = prev.iter().map(|p| (p.pid, p)).collect();
            let nm: HashMap<u32, &ProcMem> = now.iter().map(|p| (p.pid, p)).collect();
            (Some(pm), Some(nm), iv)
        }
        None => (None, None, 0.0),
    };

    if unit.is_empty() {
        // init 根行：最顶上，带自身 cpu/mem（tree 风格根节点）
        out.push_str(&init_line(prev_map.as_ref(), now_map.as_ref(), interval));
        // 列出全部服务单元（按名排序），运行实例合并其状态；
        // services 中不在 units 里的实例（手动注册）也一并列出
        let mut names: Vec<&str> = units
            .iter()
            .filter(|(_, u)| !u.is_target)
            .map(|(n, _)| n.as_str())
            .collect();
        for svc in services {
            if !names.contains(&svc.name.as_str()) {
                names.push(&svc.name);
            }
        }
        names.sort_unstable();
        for (i, name) in names.iter().enumerate() {
            let is_last = i + 1 == names.len();
            let branch = if is_last { "└── " } else { "├── " };
            match services.iter().find(|s| s.name == *name) {
                Some(svc) => {
                    let mut line = format!("{}{}", branch, svc.status_line().trim_end());
                    if let (Some(pm), Some(nm)) = (&prev_map, &now_map)
                        && let Some(tree) = service_root_tree(svc, nm)
                    {
                        // 运行实例：汇总并入服务行，进程树挂在分支下
                        let (count, mem, cpu) = summarize_tree(&tree, pm, nm, interval);
                        line.push_str(&format!(
                            " procs={} mem={} cpu={:.1}%",
                            count,
                            crate::applets::proc::human_size(mem),
                            cpu
                        ));
                        line.push('\n');
                        out.push_str(&line);
                        let prefix = if is_last { "    " } else { "│   " };
                        render_status_node(&tree, pm, nm, interval, prefix, true, &mut out);
                    } else {
                        line.push('\n');
                        out.push_str(&line);
                    }
                }
                None => out.push_str(&format!("{}{}", branch, not_started_line(name, units))),
            }
        }
    } else if unit == "init" {
        out.push_str(&init_line(prev_map.as_ref(), now_map.as_ref(), interval));
    } else if let Some(svc) = services.iter().find(|s| s.name == unit) {
        out.push_str(&svc.status_line());
        if let (Some(pm), Some(nm)) = (&prev_map, &now_map)
            && let Some(tree) = service_root_tree(svc, nm)
        {
            let (count, mem, cpu) = summarize_tree(&tree, pm, nm, interval);
            out.push_str(&format!(
                "    procs={} mem={} cpu={:.1}%\n",
                count,
                crate::applets::proc::human_size(mem),
                cpu
            ));
            render_status_node(&tree, pm, nm, interval, "    ", true, &mut out);
        }
    } else if let Some(u) = units.get(unit) {
        if u.is_target {
            out.push_str(&format!("{} target\n", unit));
        } else {
            out.push_str(&not_started_line(unit, units));
        }
    } else {
        out.push_str(&format!("unknown unit: {}\n", unit));
    }
    out
}

/// init 根行：`init pid=1`，有快照时附加自身 cpu/mem。
fn init_line(
    prev_map: Option<&HashMap<u32, &ProcMem>>,
    now_map: Option<&HashMap<u32, &ProcMem>>,
    interval: f64,
) -> String {
    let mut line = format!("init pid={}\n", std::process::id());
    if let (Some(pm), Some(nm)) = (prev_map, now_map)
        && let Some(p) = nm.get(&1)
    {
        let cpu = cpu_percent_for(1, pm, nm, interval).unwrap_or(0.0);
        line = format!(
            "init pid=1 cpu={:.1}% mem={}\n",
            cpu,
            crate::applets::proc::human_size(p.rss_kb)
        );
    }
    line
}

/// 已配置但从未运行的单元状态行（含重启策略提示）。
fn not_started_line(name: &str, units: &HashMap<String, Unit>) -> String {
    let restart = match units.get(name).map(|u| u.service.restart.as_str()) {
        Some("always") => " restart=always",
        Some("on-failure") => " restart=on-failure",
        _ => "",
    };
    format!("{} not-started{}\n", name, restart)
}

/// 服务进程树节点（status 展示用；按 ppid 从根向下构建）。
struct StatusProc {
    pid: u32,
    name: String,
    state: String,
    rss_kb: u64,
    children: Vec<StatusProc>,
}

/// 从服务根 pid（simple=主进程，forking=daemon）构建进程树；
/// 根进程不在快照中（已退出）时返回 None。
fn service_root_tree(svc: &ServiceInstance, now: &HashMap<u32, &ProcMem>) -> Option<StatusProc> {
    let root_pid = svc.child.as_ref().map(|c| c.id()).or(svc.tracked_pid)?;
    let root = now.get(&root_pid)?;
    Some(build_status_node(root, now))
}

/// 递归构建节点（子进程按 PID 升序）。
fn build_status_node(p: &ProcMem, by_pid: &HashMap<u32, &ProcMem>) -> StatusProc {
    let mut children: Vec<&ProcMem> = by_pid
        .values()
        .filter(|c| c.ppid == p.pid)
        .copied()
        .collect();
    children.sort_by_key(|c| c.pid);
    StatusProc {
        pid: p.pid,
        name: p.name.clone(),
        state: p.state.clone(),
        rss_kb: p.rss_kb,
        children: children
            .iter()
            .map(|c| build_status_node(c, by_pid))
            .collect(),
    }
}

/// 递归渲染进程树（tree 风格连接符：├──/└──/│）。
fn render_status_node(
    node: &StatusProc,
    prev: &HashMap<u32, &ProcMem>,
    now: &HashMap<u32, &ProcMem>,
    interval: f64,
    prefix: &str,
    is_last: bool,
    out: &mut String,
) {
    let cpu_str = match cpu_percent_for(node.pid, prev, now, interval) {
        Some(p) => format!("{:.1}%", p),
        None => "-".to_string(),
    };
    let branch = if is_last { "└── " } else { "├── " };
    out.push_str(&format!(
        "{}{}{} {} {} cpu={} mem={}\n",
        prefix,
        branch,
        node.pid,
        node.name,
        node.state,
        cpu_str,
        crate::applets::proc::human_size(node.rss_kb)
    ));
    let child_prefix = if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };
    for (i, child) in node.children.iter().enumerate() {
        render_status_node(
            child,
            prev,
            now,
            interval,
            &child_prefix,
            i + 1 == node.children.len(),
            out,
        );
    }
}

/// 汇总子树：进程数 / 总 RSS / 总 CPU 占用率。
fn summarize_tree(
    node: &StatusProc,
    prev: &HashMap<u32, &ProcMem>,
    now: &HashMap<u32, &ProcMem>,
    interval: f64,
) -> (u32, u64, f64) {
    let mut count = 1;
    let mut mem = node.rss_kb;
    let mut cpu = cpu_percent_for(node.pid, prev, now, interval).unwrap_or(0.0);
    for c in &node.children {
        let (n, m, c2) = summarize_tree(c, prev, now, interval);
        count += n;
        mem += m;
        cpu += c2;
    }
    (count, mem, cpu)
}

/// 进程 CPU 占用率：两次采样 cpu_ticks 差值 / (间隔 × CLK_TCK)。
/// 进程在任一快照缺失（退出/新建）时返回 None。
fn cpu_percent_for(
    pid: u32,
    prev: &HashMap<u32, &ProcMem>,
    now: &HashMap<u32, &ProcMem>,
    interval_secs: f64,
) -> Option<f64> {
    let before = prev.get(&pid)?.cpu_ticks;
    let after = now.get(&pid)?.cpu_ticks;
    let delta = after.saturating_sub(before) as f64;
    let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    Some(delta / (interval_secs * clk) * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::core::init::services::test_svc;

    #[test]
    fn format_status_lists_all() {
        let services = vec![test_svc("a.service", false), test_svc("b.service", true)];
        let out = format_status("", &services, &HashMap::new(), None);
        assert!(out.contains("init pid="), "out: {}", out);
        assert!(out.contains("a.service exited"), "out: {}", out);
        assert!(
            out.contains("b.service exited restart=on-failure"),
            "out: {}",
            out
        );
    }

    #[test]
    fn format_status_single_unit() {
        let services = vec![test_svc("a.service", false)];
        let out = format_status("a.service", &services, &HashMap::new(), None);
        assert!(out.contains("a.service exited"), "out: {}", out);
        assert!(!out.contains("init pid="), "out: {}", out);
    }

    #[test]
    fn format_status_unknown_unit() {
        let out = format_status("ghost.service", &[], &HashMap::new(), None);
        assert!(out.contains("unknown unit: ghost.service"), "out: {}", out);
    }

    #[test]
    fn format_status_restart_always() {
        // console 现在是普通服务（Restart=always），status 同样列出
        let mut svc = test_svc("console-shell.service", false);
        svc.restart_always = true;
        let out = format_status("", &[svc], &HashMap::new(), None);
        assert!(
            out.contains("console-shell.service exited restart=always"),
            "out: {}",
            out
        );
        assert!(!out.contains("console-shell stopped"), "out: {}", out);
    }

    #[test]
    fn format_status_lists_not_started_units() {
        // 配置了但从未运行的服务单元应出现在完整清单中（not-started）
        let units = units_with_names(&["foo.service", "bar.service"]);
        let out = format_status("", &[], &units, None);
        assert!(out.contains("foo.service not-started"), "out: {}", out);
        assert!(out.contains("bar.service not-started"), "out: {}", out);
    }

    #[test]
    fn format_status_single_not_started_unit() {
        let units = units_with_names(&["foo.service"]);
        let out = format_status("foo.service", &[], &units, None);
        assert!(out.contains("foo.service not-started"), "out: {}", out);
        assert!(!out.contains("init pid="), "out: {}", out);
    }

    #[test]
    fn format_status_target() {
        let mut units = units_with_names(&["default.target"]);
        units.get_mut("default.target").unwrap().is_target = true;
        let out = format_status("default.target", &[], &units, None);
        assert!(out.contains("default.target target"), "out: {}", out);
        // 完整清单不列出 target
        let out = format_status("", &[], &units, None);
        assert!(!out.contains("default.target"), "out: {}", out);
    }

    #[test]
    fn format_status_merges_running_and_units() {
        // 运行实例 + 未启动单元同时呈现
        let mut units = units_with_names(&["a.service", "c.service"]);
        units.get_mut("a.service").unwrap().service.restart = "always".to_string();
        let mut svc = test_svc("b.service", false);
        svc.child = Some(std::process::Command::new("true").spawn().unwrap());
        let out = format_status("", &[svc], &units, None);
        assert!(
            out.contains("a.service not-started restart=always"),
            "out: {}",
            out
        );
        assert!(out.contains("b.service running"), "out: {}", out);
        assert!(out.contains("c.service not-started"), "out: {}", out);
    }

    #[test]
    fn status_line_shows_fail_count() {
        let mut svc = test_svc("a.service", false);
        svc.fail_count = 3;
        svc.start_limit_burst = 5;
        let out = svc.status_line();
        assert!(out.contains("a.service exited failed=3/5"), "out: {}", out);
    }

    /// 构造 ProcMem 快照项（name/state/rss/cpu_ticks）。
    fn pmem(pid: u32, ppid: u32, name: &str, rss_kb: u64, cpu: u64) -> ProcMem {
        ProcMem {
            pid,
            ppid,
            vsz_kb: rss_kb * 2,
            rss_kb,
            state: "S".into(),
            name: name.into(),
            exe: format!("/bin/{}", name),
            cpu_ticks: cpu,
        }
    }

    #[test]
    fn status_renders_service_proc_tree() {
        // rgetty(49) -> rlogin(120) -> sh(130)；rlogin 增量 0 -> cpu=0.0%
        let prev = vec![
            pmem(49, 1, "rgetty", 1200, 100),
            pmem(120, 49, "rlogin", 800, 50),
            pmem(130, 120, "sh", 1000, 0),
        ];
        let now = vec![
            pmem(49, 1, "rgetty", 1200, 130),
            pmem(120, 49, "rlogin", 800, 50),
            pmem(130, 120, "sh", 1000, 0),
        ];
        let mut svc = test_svc("console.service", false);
        svc.tracked_pid = Some(49); // 模拟 forking 根
        let out = format_status("", &[svc], &HashMap::new(), Some((&prev, &now, 1.0)));
        // tree 风格：init 在最顶，服务为唯一分支（└──），进程树挂其下
        assert!(out.starts_with("init pid="), "out: {}", out);
        assert!(
            out.contains("└── console.service running pid=49 procs=3 mem=2.9M cpu=30.0%"),
            "out: {}",
            out
        );
        assert!(
            out.contains("    └── 49 rgetty S cpu=30.0% mem=1.2M"),
            "out: {}",
            out
        );
        assert!(out.contains("        └── 120 rlogin S"), "out: {}", out);
        assert!(out.contains("cpu=0.0% mem=1000K"), "out: {}", out); // sh
        assert!(out.contains("└── 130 sh S"), "out: {}", out);
    }

    #[test]
    fn status_tree_connectors_and_init_line() {
        // 多服务：非最后 ├──、最后 └──；init 行带自身 cpu/mem
        let prev = vec![pmem(1, 0, "init", 3000, 10), pmem(10, 1, "svc1", 100, 0)];
        let now = vec![pmem(1, 0, "init", 3000, 10), pmem(10, 1, "svc1", 100, 0)];
        let mut a = test_svc("a.service", false);
        a.tracked_pid = Some(10);
        let b = test_svc("b.service", false);
        let out = format_status("", &[a, b], &HashMap::new(), Some((&prev, &now, 1.0)));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("init pid=1 cpu="), "out: {}", out);
        assert!(lines[0].contains("mem=2.9M"), "out: {}", out);
        assert!(lines[1].starts_with("├── a.service"), "out: {}", out);
        assert!(lines[2].starts_with("│   └── 10"), "out: {}", out);
        assert!(lines[3].starts_with("└── b.service exited"), "out: {}", out);
    }

    #[test]
    fn status_skips_procs_when_root_missing() {
        // 根进程不在快照中（已退出）-> 不渲染树，仅状态行
        let now = vec![pmem(999, 1, "other", 100, 0)];
        let mut svc = test_svc("console.service", false);
        svc.tracked_pid = Some(49);
        let out = format_status("", &[svc], &HashMap::new(), Some((&now, &now, 1.0)));
        assert!(
            out.contains("console.service running pid=49"),
            "out: {}",
            out
        );
        assert!(!out.contains("procs="), "out: {}", out);
    }

    #[test]
    fn cpu_percent_uses_delta_over_interval() {
        // 增量 30 ticks / (1s * CLK_TCK) * 100：精确值依赖 CLK_TCK，
        // 这里验证缺失 pid 返回 None 与 0 增量返回 0.0%
        let prev = [pmem(1, 0, "a", 10, 10)];
        let now = [pmem(1, 0, "a", 10, 10)];
        let pm: HashMap<u32, &ProcMem> = prev.iter().map(|p| (p.pid, p)).collect();
        let nm: HashMap<u32, &ProcMem> = now.iter().map(|p| (p.pid, p)).collect();
        assert_eq!(cpu_percent_for(1, &pm, &nm, 1.0), Some(0.0));
        assert_eq!(cpu_percent_for(2, &pm, &nm, 1.0), None); // 缺失
        assert!(cpu_percent_for(1, &pm, &nm, 1.0).unwrap() < 1.0);
    }

    /// 构造仅含给定名称单元的 map（is_target=false，无 ExecStart）。
    fn units_with_names(names: &[&str]) -> HashMap<String, Unit> {
        names
            .iter()
            .map(|n| {
                let u = Unit {
                    name: n.to_string(),
                    ..Default::default()
                };
                (n.to_string(), u)
            })
            .collect()
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

    #[test]
    fn parse_control_request_new_commands() {
        assert_eq!(
            parse_control_request("enable foo"),
            Ok(ControlRequest::Enable("foo"))
        );
        assert_eq!(
            parse_control_request("disable foo"),
            Ok(ControlRequest::Disable("foo"))
        );
        assert_eq!(
            parse_control_request("is-enabled foo"),
            Ok(ControlRequest::IsEnabled("foo"))
        );
        assert_eq!(
            parse_control_request("isolate multi-user.target"),
            Ok(ControlRequest::Isolate("multi-user.target"))
        );
        assert_eq!(
            parse_control_request("reset-failed"),
            Ok(ControlRequest::ResetFailed(None))
        );
        assert_eq!(
            parse_control_request("reset-failed foo"),
            Ok(ControlRequest::ResetFailed(Some("foo")))
        );
        assert!(parse_control_request("enable").is_err());
    }
}
