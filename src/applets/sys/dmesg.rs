//! `dmesg` - 查看内核环形缓冲区日志。
//!
//! 用法：dmesg [-n N] [-c]
//! - `-n N`：只显示最后 N 条（默认全部）；
//! - `-c`：读取后清空内核环形缓冲区。
//!
//! 优先用 `klogctl(SYSLOG_ACTION_READ_ALL/READ_CLEAR)` 读取完整环形缓冲区
//! （与真实 dmesg 一致，可重复读取）；无权限或旧内核时回退到 /dev/kmsg
//! 非阻塞读取。输出去除 `<pri>` 级别前缀，仅保留消息正文。

use crate::applet::Applet;
use std::io::Read;
use std::process::ExitCode;

pub struct Dmesg;
pub static DMESG: &Dmesg = &Dmesg;

/// 单条 kmsg 记录可能超过 8KB（扩 buffer 重试上限）。
const MAX_BUF: usize = 1024 * 1024;

/// syslog(2) 动作码（内核 uapi/linux/syslog.h）。
const SYSLOG_ACTION_READ_ALL: libc::c_int = 3;
const SYSLOG_ACTION_READ_CLEAR: libc::c_int = 4;
const SYSLOG_ACTION_SIZE_BUFFER: libc::c_int = 10;

/// 去除 `<pri>` 级别前缀（klogctl 输出格式）。
pub(crate) fn strip_level_prefix(line: &str) -> String {
    let rest = line.strip_prefix('<');
    if let Some(rest) = rest
        && let Some(end) = rest.find('>')
        && rest[..end].bytes().all(|b| b.is_ascii_digit())
    {
        return rest[end + 1..].to_string();
    }
    line.to_string()
}

/// 去除 /dev/kmsg 记录前缀（`<pri>,seq,ts,flags;`），返回消息正文。
pub(crate) fn format_kmsg_line(record: &str) -> String {
    match record.find(';') {
        Some(i) => record[i + 1..].to_string(),
        None => record.to_string(),
    }
}

/// 取最后 n 条（n=0 表示全部）。
pub(crate) fn tail_n(lines: &[String], n: usize) -> Vec<String> {
    if n == 0 || n >= lines.len() {
        lines.to_vec()
    } else {
        lines[lines.len() - n..].to_vec()
    }
}

/// 用 klogctl 读取完整环形缓冲区（`clear=true` 时读取并清空）。
pub(crate) fn read_klog(clear: bool) -> std::io::Result<String> {
    let size = unsafe { libc::klogctl(SYSLOG_ACTION_SIZE_BUFFER, std::ptr::null_mut(), 0) };
    if size <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut buf = vec![0u8; size as usize];
    let action = if clear {
        SYSLOG_ACTION_READ_CLEAR
    } else {
        SYSLOG_ACTION_READ_ALL
    };
    let n = unsafe {
        libc::klogctl(
            action,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len() as libc::c_int,
        )
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 回退路径：非阻塞读取 /dev/kmsg 中当前可用的全部记录。
pub(crate) fn read_kmsg(path: &str) -> std::io::Result<Vec<String>> {
    let mut f = std::fs::File::open(path)?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&f);
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
    let mut buf = vec![0u8; 8192];
    let mut out: Vec<String> = Vec::new();
    loop {
        match f.read(&mut buf) {
            Ok(0) => break, // kmsg 无 EOF，防御性处理
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                for line in text.lines() {
                    out.push(format_kmsg_line(line));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                // 单条记录大于当前 buffer：扩大后重试
                if buf.len() >= MAX_BUF {
                    return Err(e);
                }
                buf.resize(buf.len() * 2, 0);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// 读取全部日志行：优先 klogctl，失败回退 /dev/kmsg。
pub(crate) fn read_messages(clear: bool) -> std::io::Result<Vec<String>> {
    match read_klog(clear) {
        Ok(text) => Ok(text
            .lines()
            .map(strip_level_prefix)
            .filter(|l| !l.trim().is_empty())
            .collect()),
        Err(klog_err) => {
            let records = read_kmsg("/dev/kmsg").map_err(|_| klog_err)?;
            Ok(records
                .into_iter()
                .filter(|l| !l.trim().is_empty())
                .collect())
        }
    }
}

impl Applet for Dmesg {
    fn name(&self) -> &'static str {
        "dmesg"
    }
    fn help(&self) -> &'static str {
        "dmesg [-n N] [-c] - show kernel ring buffer messages"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut count: usize = 0;
        let mut clear = false;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-n" => {
                    i += 1;
                    let Some(v) = args.get(i) else {
                        eprintln!("dmesg: option -n requires an argument");
                        return ExitCode::FAILURE;
                    };
                    match v.parse::<usize>() {
                        Ok(n) => count = n,
                        Err(_) => {
                            eprintln!("dmesg: invalid count '{}'", v);
                            return ExitCode::FAILURE;
                        }
                    }
                }
                "-c" => clear = true,
                other => {
                    eprintln!("dmesg: unknown option: {}", other);
                    return ExitCode::FAILURE;
                }
            }
            i += 1;
        }

        let messages = match read_messages(clear) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("dmesg: cannot read kernel log: {}", e);
                return ExitCode::FAILURE;
            }
        };
        for line in tail_n(&messages, count) {
            println!("{}", line);
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(DMESG.name(), "dmesg");
        assert!(DMESG.help().contains("kernel"));
    }

    #[test]
    fn strips_level_prefix() {
        assert_eq!(strip_level_prefix("<6>rbox I: hello"), "rbox I: hello");
        assert_eq!(strip_level_prefix("<14>message"), "message");
        assert_eq!(strip_level_prefix("no prefix"), "no prefix");
        // 非数字前缀不剥离
        assert_eq!(strip_level_prefix("<abc>text"), "<abc>text");
        // 无 '>' 不剥离
        assert_eq!(strip_level_prefix("<6text"), "<6text");
    }

    #[test]
    fn strips_kmsg_prefix() {
        assert_eq!(
            format_kmsg_line("6,123,456789,-;hello kernel"),
            "hello kernel"
        );
        assert_eq!(format_kmsg_line("no-prefix-here"), "no-prefix-here");
        assert_eq!(format_kmsg_line("3,1,2,;"), "");
    }

    #[test]
    fn tail_n_keeps_last() {
        let lines: Vec<String> = (1..=5).map(|i| i.to_string()).collect();
        assert_eq!(tail_n(&lines, 2), vec!["4", "5"]);
        assert_eq!(tail_n(&lines, 0), lines);
        assert_eq!(tail_n(&lines, 99), lines);
        assert_eq!(tail_n(&[], 3), Vec::<String>::new());
    }

    #[test]
    fn unknown_option_fails() {
        assert_eq!(DMESG.run(&["-x".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn missing_count_argument_fails() {
        assert_eq!(DMESG.run(&["-n".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn invalid_count_fails() {
        assert_eq!(
            DMESG.run(&["-n".to_string(), "abc".to_string()]),
            ExitCode::FAILURE
        );
    }
}
