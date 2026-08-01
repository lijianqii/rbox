//! `rservice` - 服务管理：列出/启动/停止/重启/重载服务。
//!
//! 通过 unix socket (/tmp/rbox.sock) 与 PID 1 通信：
//! - `rservice` / `rservice list`             列出所有服务状态
//! - `rservice status [unit]`                 查询单个服务
//! - `rservice start|stop|restart|reload <unit>` 管理服务

use crate::applet::Applet;
use crate::applets::core::control::send_request;
use std::io::Write;
use std::process::ExitCode;

pub struct Rservice;
pub static RSERVICE: &Rservice = &Rservice;

impl Applet for Rservice {
    fn name(&self) -> &'static str {
        "rservice"
    }
    fn help(&self) -> &'static str {
        "rservice [list|status [unit]|start|stop|restart|reload <unit>] - manage services"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        // 构造请求行
        let req = if args.is_empty() || (args.len() == 1 && args[0] == "list") {
            "status".to_string()
        } else if args.len() == 2
            && matches!(
                args[0].as_str(),
                "start" | "stop" | "restart" | "reload" | "status"
            )
        {
            format!("{} {}", args[0], args[1])
        } else if args.len() == 1
            && matches!(args[0].as_str(), "start" | "stop" | "restart" | "reload")
        {
            eprintln!("rservice: {} requires a unit name", args[0]);
            return ExitCode::from(2);
        } else {
            eprintln!(
                "rservice: usage: rservice [list|status [unit]|start|stop|restart|reload <unit>]"
            );
            return ExitCode::from(2);
        };

        match send_request(&req) {
            Ok(resp) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(resp.as_bytes());
                let _ = out.flush();
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rservice: {}", e);
                ExitCode::from(1)
            }
        }
    }
}
