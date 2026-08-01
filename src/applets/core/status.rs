//! `status` - 查询 init 服务状态。
//!
//! 通过 unix socket (/tmp/rbox.sock) 与 PID 1 通信：
//! - `rbox status`           列出所有服务状态
//! - `rbox status <unit>`    查询单个单元

use crate::applet::Applet;
use crate::applets::core::control::send_request;
use std::io::Write;
use std::process::ExitCode;

pub struct Status;
pub static STATUS: &Status = &Status;

impl Applet for Status {
    fn name(&self) -> &'static str {
        "status"
    }
    fn help(&self) -> &'static str {
        "status [unit] - query service status from init"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let req = match args.first() {
            Some(name) => format!("status {}", name),
            None => "status".to_string(),
        };
        match send_request(&req) {
            Ok(resp) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(resp.as_bytes());
                let _ = out.flush();
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("status: {}", e);
                ExitCode::from(1)
            }
        }
    }
}
