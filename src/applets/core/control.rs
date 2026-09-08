//! init 控制协议客户端（status/rservice 共用）。
//!
//! 与 init（PID 1）通过 unix socket 通信：发送一行请求，读取文本响应。

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

/// 控制协议 socket 路径（/etc/rbox.conf [paths] status_socket，默认 /run/rbox.sock）。
pub fn status_socket() -> String {
    crate::config::load().paths.status_socket.clone()
}

/// 读响应超时（秒）：init 无响应或宿主机残留 socket 时避免 rservice/status 挂死。
/// 正常响应为毫秒级，5s 足够；超时返回 Err。
pub const REQUEST_TIMEOUT_SECS: u64 = 5;

/// 发送一行控制请求并读取完整响应。
pub fn send_request(req: &str) -> Result<String, String> {
    send_request_to(
        &status_socket(),
        req,
        std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
    )
}

/// 向指定路径的 socket 发送请求并读取响应（路径/超时参数化，便于测试）。
fn send_request_to(path: &str, req: &str, timeout: std::time::Duration) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(path).map_err(|e| format!("cannot connect to init: {}", e))?;
    stream
        .write_all(format!("{}\n", req).as_bytes())
        .map_err(|_| "write failed".to_string())?;
    // 读响应带超时：connect 成功但服务端不响应（残留 socket / init 卡住）时
    // 不会永久阻塞，超时后返回错误
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set timeout failed: {}", e))?;
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

    #[test]
    fn send_request_times_out_when_server_silent() {
        // 模拟残留 socket：server accept 后不响应——客户端应超时返回而非永久挂死
        let dir = format!("/tmp/rbox_ctrl_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{}/test.sock", dir);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let _ = listener.accept(); // 接受连接但不发送任何数据
        });
        let start = std::time::Instant::now();
        let result = send_request_to(&path, "status", std::time::Duration::from_millis(300));
        assert!(result.is_err(), "应超时返回错误: {:?}", result);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "应在超时后返回而非挂死"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_request_reads_response() {
        // server 正常响应：客户端读到完整响应
        let dir = format!("/tmp/rbox_ctrl2_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{}/ok.sock", dir);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf); // 读请求
                let _ = stream.write_all(b"hello from server\n");
                let _ = stream.flush();
                // 保持连接短暂时间，避免立即关闭导致客户端读到 RST
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        let result = send_request_to(&path, "status", std::time::Duration::from_secs(2));
        assert_eq!(result.as_deref(), Ok("hello from server\n"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
