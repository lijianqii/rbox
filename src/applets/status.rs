//! `status` - 查询 init 服务状态。
//!
//! 通过 unix socket (/tmp/rbox.sock) 与 PID 1 通信：
//! - `rbox status`           列出所有服务状态
//! - `rbox status <unit>`    查询单个单元（如 hello.service）

use crate::applet::Applet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

/// 与 init.rs 的 STATUS_SOCKET 保持一致。
const STATUS_SOCKET: &str = "/tmp/rbox.sock";

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
        let mut stream = match UnixStream::connect(STATUS_SOCKET) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("status: cannot connect to init: {}", e);
                return ExitCode::from(1);
            }
        };
        let req = match args.first() {
            Some(name) => format!("status {}\n", name),
            None => "status\n".to_string(),
        };
        if stream.write_all(req.as_bytes()).is_err() {
            eprintln!("status: write failed");
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
                eprintln!("status: read error: {}", e);
                ExitCode::from(1)
            }
        }
    }
}
