//! init 控制协议服务端：监听 unix socket，处理 status/start/stop/restart/reload。

use crate::applets::core::control::STATUS_SOCKET;
use crate::applets::core::log;
use crate::applets::core::init::services::{
    parse_environment, respawn_service, start_forking_service, start_service,
    stop_service_instance, ServiceInstance,
};
use crate::applets::core::init::units::{parse_cmdline, Unit};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Child;

/// 创建控制 socket（非阻塞）；失败时返回 None（不影响启动）。
pub(crate) fn create_status_listener() -> Option<UnixListener> {
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

/// 处理一次控制连接：读一行请求，分发到 status/start/stop/restart/reload，回写响应，关闭。
/// 读请求带 100ms 超时，避免异常客户端挂住主循环。
pub(crate) fn handle_control_connection(
    mut stream: UnixStream,
    console_name: &str,
    console: Option<&Child>,
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
    console_name: &str,
    console: Option<&Child>,
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
    if svc.child.is_none() && svc.tracked_pid.is_none() {
        return format!("{} already stopped\n", name);
    }
    stop_service_instance(svc);
    format!("{} stopped\n", name)
}

/// 生成 status 响应文本。请求为空时列出全部；`status <unit>` 查单个。
fn format_status(
    req: &str,
    console_name: &str,
    console: Option<&Child>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::core::init::services::test_svc;

    #[test]
    fn format_status_lists_all() {
        let services = vec![test_svc("a.service", false), test_svc("b.service", true)];
        let out = format_status("", "console-shell.service", None, &services);
        assert!(out.contains("init pid="), "out: {}", out);
        assert!(out.contains("console-shell.service stopped"), "out: {}", out);
        assert!(out.contains("a.service exited"), "out: {}", out);
        assert!(out.contains("b.service exited restart=on-failure"), "out: {}", out);
    }

    #[test]
    fn format_status_single_unit() {
        let services = vec![test_svc("a.service", false)];
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
        assert_eq!(parse_control_request("status"), Ok(ControlRequest::Status(None)));
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
        assert!(parse_control_request("reload").unwrap_err().contains("usage"));
    }
}
