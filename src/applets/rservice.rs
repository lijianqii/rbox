//! `rservice` - 服务管理：列出/启动/停止/重启服务。
//!
//! 通过 unix socket (/tmp/rbox.sock) 与 PID 1 通信：
//! - `rservice` / `rservice list`             列出所有服务状态
//! - `rservice status [unit]`                 查询单个服务
//! - `rservice start|stop|restart <unit>`     启动/停止/重启服务

use crate::applet::Applet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

/// 与 init.rs 的 STATUS_SOCKET 保持一致。
const STATUS_SOCKET: &str = "/tmp/rbox.sock";

pub struct Rservice;
pub static RSERVICE: &Rservice = &Rservice;

impl Applet for Rservice {
    fn name(&self) -> &'static str {
        "rservice"
    }
    fn help(&self) -> &'static str {
        "rservice [list|status [unit]|start|stop|restart <unit>] - manage services"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        // 构造请求行
        let req = if args.is_empty() || (args.len() == 1 && args[0] == "list") {
            "status".to_string()
        } else if args.len() == 2
            && matches!(args[0].as_str(), "start" | "stop" | "restart" | "status")
        {
            format!("{} {}", args[0], args[1])
        } else if args.len() == 1 && matches!(args[0].as_str(), "start" | "stop" | "restart") {
            eprintln!("rservice: {} requires a unit name", args[0]);
            return ExitCode::from(2);
        } else {
            eprintln!("rservice: usage: rservice [list|status [unit]|start|stop|restart <unit>]");
            return ExitCode::from(2);
        };

        let mut stream = match UnixStream::connect(STATUS_SOCKET) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rservice: cannot connect to init: {}", e);
                return ExitCode::from(1);
            }
        };
        if stream.write_all(format!("{}\n", req).as_bytes()).is_err() {
            eprintln!("rservice: write failed");
            return ExitCode::from(1);
        }
        let mut resp = String::new();
        match stream.read_to_string(&mut resp) {
            Ok(_) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(resp.as_bytes());
                let _ = out.flush();
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rservice: read error: {}", e);
                ExitCode::from(1)
            }
        }
    }
}
