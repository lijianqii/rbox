//! init 控制协议客户端（status/rservice 共用）。
//!
//! 与 init（PID 1）通过 unix socket 通信：发送一行请求，读取文本响应。

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

/// 控制协议 socket 路径（服务端 init 与客户端 status/rservice 保持一致）。
pub const STATUS_SOCKET: &str = "/tmp/rbox.sock";

/// 发送一行控制请求并读取完整响应。
pub fn send_request(req: &str) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(STATUS_SOCKET).map_err(|e| format!("cannot connect to init: {}", e))?;
    stream
        .write_all(format!("{}\n", req).as_bytes())
        .map_err(|_| "write failed".to_string())?;
    let mut resp = String::new();
    stream
        .read_to_string(&mut resp)
        .map_err(|e| format!("read error: {}", e))?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_request_no_socket() {
        // No init running -> connection should fail
        let result = send_request("status");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot connect"));
    }
}
