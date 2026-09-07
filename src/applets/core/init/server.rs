//! init 控制协议服务端：监听 unix socket，处理 status/start/stop/restart/reload。

use crate::applets::core::control::status_socket;
use crate::applets::core::init::services::{
    EXEC_COMMAND_TIMEOUT, ServiceInstance, parse_environment, respawn_service,
    run_command_with_timeout, start_forking_service, start_service, stop_service_instance,
};
use crate::applets::core::init::units::{Unit, parse_cmdline};
use crate::applets::core::log;
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
    units: std::sync::Arc<HashMap<String, Unit>>,
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
    let resp = match parse_control_request(&req) {
        Ok(r) => match services.lock() {
            Ok(mut guard) => execute_control_request(r, &mut guard, &units),
            Err(_) => "error: service state poisoned\n".to_string(),
        },
        Err(e) => format!("error: {}\n", e),
    };
    // 读阶段用非阻塞避免挂住线程；写响应时恢复阻塞，避免小响应遇 WouldBlock 丢失
    let _ = stream.set_nonblocking(false);
    let _ = stream.write_all(resp.as_bytes());
}

/// 控制请求：status 查询或服务管理命令。
#[derive(Debug, PartialEq)]
pub(crate) enum ControlRequest<'a> {
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
    services: &mut Vec<ServiceInstance>,
    units: &HashMap<String, Unit>,
) -> String {
    match req {
        ControlRequest::Status(unit) => format_status(unit.unwrap_or(""), services, units),
        ControlRequest::Start(name) => do_start(name, services, units),
        ControlRequest::Stop(name) => do_stop(name, services),
        ControlRequest::Reload(name) => do_reload(name, services),
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
fn do_stop(name: &str, services: &mut [ServiceInstance]) -> String {
    let svc = match services.iter_mut().find(|s| s.name == name) {
        Some(s) => s,
        None => return format!("unknown unit: {}\n", name),
    };
    svc.stopped = true;
    if svc.child.is_none() && svc.tracked_pid.is_none() {
        return format!("{} already stopped\n", name);
    }
    stop_service_instance(svc);
    format!("{} stopped\n", name)
}

/// 生成 status 响应文本。`unit` 为空列出全部（含未启动单元）；否则查单个单元。
/// 未启动单元显示 not-started，与运行实例（services）状态合并呈现。
fn format_status(
    unit: &str,
    services: &[ServiceInstance],
    units: &HashMap<String, Unit>,
) -> String {
    let unit = unit.trim();
    let mut out = String::new();

    if unit.is_empty() {
        out.push_str(&format!("init pid={}\n", std::process::id()));
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
        for name in names {
            match services.iter().find(|s| s.name == name) {
                Some(svc) => out.push_str(&svc.status_line()),
                None => out.push_str(&not_started_line(name, units)),
            }
        }
    } else if unit == "init" {
        out.push_str(&format!("init pid={}\n", std::process::id()));
    } else if let Some(svc) = services.iter().find(|s| s.name == unit) {
        out.push_str(&svc.status_line());
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

/// 已配置但从未运行的单元状态行（含重启策略提示）。
fn not_started_line(name: &str, units: &HashMap<String, Unit>) -> String {
    let restart = match units.get(name).map(|u| u.service.restart.as_str()) {
        Some("always") => " restart=always",
        Some("on-failure") => " restart=on-failure",
        _ => "",
    };
    format!("{} not-started{}\n", name, restart)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::core::init::services::test_svc;

    #[test]
    fn format_status_lists_all() {
        let services = vec![test_svc("a.service", false), test_svc("b.service", true)];
        let out = format_status("", &services, &HashMap::new());
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
        let out = format_status("a.service", &services, &HashMap::new());
        assert!(out.contains("a.service exited"), "out: {}", out);
        assert!(!out.contains("init pid="), "out: {}", out);
    }

    #[test]
    fn format_status_unknown_unit() {
        let out = format_status("ghost.service", &[], &HashMap::new());
        assert!(out.contains("unknown unit: ghost.service"), "out: {}", out);
    }

    #[test]
    fn format_status_restart_always() {
        // console 现在是普通服务（Restart=always），status 同样列出
        let mut svc = test_svc("console-shell.service", false);
        svc.restart_always = true;
        let out = format_status("", &[svc], &HashMap::new());
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
        let out = format_status("", &[], &units);
        assert!(out.contains("foo.service not-started"), "out: {}", out);
        assert!(out.contains("bar.service not-started"), "out: {}", out);
    }

    #[test]
    fn format_status_single_not_started_unit() {
        let units = units_with_names(&["foo.service"]);
        let out = format_status("foo.service", &[], &units);
        assert!(out.contains("foo.service not-started"), "out: {}", out);
        assert!(!out.contains("init pid="), "out: {}", out);
    }

    #[test]
    fn format_status_target() {
        let mut units = units_with_names(&["default.target"]);
        units.get_mut("default.target").unwrap().is_target = true;
        let out = format_status("default.target", &[], &units);
        assert!(out.contains("default.target target"), "out: {}", out);
        // 完整清单不列出 target
        let out = format_status("", &[], &units);
        assert!(!out.contains("default.target"), "out: {}", out);
    }

    #[test]
    fn format_status_merges_running_and_units() {
        // 运行实例 + 未启动单元同时呈现
        let mut units = units_with_names(&["a.service", "c.service"]);
        units.get_mut("a.service").unwrap().service.restart = "always".to_string();
        let mut svc = test_svc("b.service", false);
        svc.child = Some(std::process::Command::new("true").spawn().unwrap());
        let out = format_status("", &[svc], &units);
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

    /// 构造仅含给定名称单元的 map（is_target=false，无 ExecStart）。
    fn units_with_names(names: &[&str]) -> HashMap<String, Unit> {
        names
            .iter()
            .map(|n| {
                let mut u = Unit::default();
                u.name = n.to_string();
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
}
