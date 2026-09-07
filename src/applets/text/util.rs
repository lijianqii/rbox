//! 文本处理共享工具（head/tail 共用）。
use std::io::{self, Read, Write};

/// 解析 `[-n N]` 与文件列表；`-` 视为 stdin（忽略）。
/// 返回 (N, 文件列表, 参数是否合法)；`-n` 缺参、数值非法、未知选项时
/// 打印错误并置 `ok=false`（调用方据此返回非零退出码）。
pub(crate) fn parse_n_files<'a>(args: &'a [String], app: &str) -> (usize, Vec<&'a String>, bool) {
    let mut n: usize = 10;
    let mut files: Vec<&String> = Vec::new();
    let mut ok = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                i += 1;
                if i < args.len() {
                    match args[i].parse() {
                        Ok(v) => n = v,
                        Err(_) => {
                            eprintln!("{}: invalid number: {}", app, args[i]);
                            ok = false;
                        }
                    }
                } else {
                    eprintln!("{}: option requires an argument: -n", app);
                    ok = false;
                }
            }
            s if s.starts_with("-n") && s.len() > 2 => match s[2..].parse() {
                Ok(v) => n = v,
                Err(_) => {
                    eprintln!("{}: invalid number: {}", app, &s[2..]);
                    ok = false;
                }
            },
            "-" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("{}: unknown option: {}", app, s);
                ok = false;
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }
    (n, files, ok)
}

/// 读取文件内容到 Vec。
/// 处理两类流式设备问题：
/// - `/dev/kmsg` 等要求"单次 read 的 buffer ≥ 消息长度"的设备：EINVAL 时扩大重试（上限 1MB）；
/// - 非普通文件（字符/块设备，无 EOF 概念）：O_NONBLOCK 读取，EAGAIN（暂时无数据）即结束。
pub(crate) fn read_file_fully(path: &str) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let mut f = std::fs::File::open(path)?;
    // 非普通文件（如 /dev/kmsg）没有 EOF：非阻塞读，EAGAIN 即结束
    if !f.metadata()?.is_file() {
        let fd = f.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        }
    }
    let mut buf = vec![0u8; 8192];
    let mut out = Vec::new();
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                if buf.len() >= 1024 * 1024 {
                    return Err(e);
                }
                buf.resize(buf.len() * 2, 0);
            }
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// 遍历输入：无文件读 stdin，多文件打印 `==> name <==` 头，逐个调用 f。
/// 返回 `false` 表示有 I/O 错误（用于退出码）。
pub(crate) fn each_input(
    files: &[&String],
    app: &str,
    out: &mut std::io::StdoutLock,
    mut f: impl FnMut(&str, &mut std::io::StdoutLock),
) -> bool {
    let mut ok = true;
    if files.is_empty() {
        let mut buf = Vec::new();
        if std::io::stdin().lock().read_to_end(&mut buf).is_ok() {
            let content = String::from_utf8_lossy(&buf);
            f(&content, out);
        } else {
            ok = false;
        }
    } else {
        for file in files {
            if files.len() > 1 {
                let _ = writeln!(out, "==> {} <==", file);
            }
            match read_file_fully(file) {
                Ok(bytes) => {
                    let content = String::from_utf8_lossy(&bytes);
                    f(&content, out);
                }
                Err(e) => {
                    eprintln!("{}: {}: {}", app, file, e);
                    ok = false;
                }
            }
        }
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_n_default() {
        let args: Vec<String> = vec![];
        let (n, files, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 10);
        assert!(files.is_empty());
        assert!(ok);
    }

    #[test]
    fn parse_n_explicit() {
        let args = vec!["-n".to_string(), "5".to_string()];
        let (n, _, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 5);
        assert!(ok);
    }

    #[test]
    fn parse_n_combined() {
        let args = vec!["-n3".to_string()];
        let (n, _, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 3);
        assert!(ok);
    }

    #[test]
    fn parse_n_missing_arg_is_error() {
        let args = vec!["-n".to_string()];
        let (n, _, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 10); // 保持默认
        assert!(!ok);
    }

    #[test]
    fn parse_n_invalid_number_is_error() {
        let args = vec!["-n".to_string(), "abc".to_string()];
        let (n, _, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 10);
        assert!(!ok);
    }

    #[test]
    fn parse_n_invalid_attached_is_error() {
        let args = vec!["-nabc".to_string()];
        let (n, _, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 10);
        assert!(!ok);
    }

    #[test]
    fn parse_unknown_option_is_error() {
        let args = vec!["-x".to_string()];
        let (_, _, ok) = parse_n_files(&args, "head");
        assert!(!ok);
    }

    #[test]
    fn parse_stdin_dash_ok() {
        let args = vec!["-".to_string()];
        let (n, files, ok) = parse_n_files(&args, "head");
        assert_eq!(n, 10);
        assert!(files.is_empty()); // "-" 被忽略
        assert!(ok);
    }

    #[test]
    fn parse_files() {
        let args = vec![
            "-n".to_string(),
            "2".to_string(),
            "a.txt".to_string(),
            "b.txt".to_string(),
        ];
        let (n, files, _ok) = parse_n_files(&args, "head");
        assert_eq!(n, 2);
        assert_eq!(files.len(), 2);
    }
}
