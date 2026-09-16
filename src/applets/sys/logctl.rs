//! `logctl` - 查询/跟踪日志文件（journalctl 风格，数据源为 logkeeper 转存的 kmsg）。
//!
//! 用法：`logctl [-F FILE] [-u UNIT] [-n N] [-p PRIO] [-f] [PATTERN]`
//! - `-F FILE` 日志文件（缺省 /var/log/messages）
//! - `-u UNIT` 仅显示包含该单元名的行
//! - `-n N`    仅显示最后 N 行（与 -f 连用时先打印末尾 N 行再跟随）
//! - `-p PRIO` 仅显示 syslog 优先级 ≤ PRIO 的行（无 `<N>` 前缀的行视为通过）
//! - `-f`      跟随输出（轮询新内容，Ctrl-C 退出）
//! - `PATTERN` 子串过滤

use crate::applet::Applet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::ExitCode;

pub struct Logctl;
pub static LOGCTL: &Logctl = &Logctl;

impl Applet for Logctl {
    fn name(&self) -> &'static str {
        "logctl"
    }
    fn help(&self) -> &'static str {
        "logctl [-F FILE] [-u UNIT] [-n N] [-p PRIO] [-f] [PATTERN] - query/tail logs"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut file = "/var/log/messages".to_string();
        let mut unit: Option<String> = None;
        let mut pattern: Option<String> = None;
        let mut last_n: Option<usize> = None;
        let mut follow = false;
        let mut prio: Option<u8> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-F" | "--file" => {
                    i += 1;
                    let Some(f) = args.get(i) else {
                        eprintln!("logctl: -F requires an argument");
                        return ExitCode::from(2);
                    };
                    file = f.clone();
                }
                "-u" | "--unit" => {
                    i += 1;
                    let Some(u) = args.get(i) else {
                        eprintln!("logctl: -u requires an argument");
                        return ExitCode::from(2);
                    };
                    unit = Some(u.clone());
                }
                "-n" | "--lines" => {
                    i += 1;
                    let Some(n) = args.get(i).and_then(|v| v.parse::<usize>().ok()) else {
                        eprintln!("logctl: -n requires a number");
                        return ExitCode::from(2);
                    };
                    last_n = Some(n);
                }
                "-p" | "--priority" => {
                    i += 1;
                    let Some(p) = args.get(i).and_then(|v| v.parse::<u8>().ok()) else {
                        eprintln!("logctl: -p requires a number 0..7");
                        return ExitCode::from(2);
                    };
                    prio = Some(p);
                }
                "-f" | "--follow" => follow = true,
                other if other.starts_with('-') && other.len() > 1 => {
                    eprintln!("logctl: unknown option: {}", other);
                    return ExitCode::from(2);
                }
                other => pattern = Some(other.to_string()),
            }
            i += 1;
        }
        let matches = |line: &str| -> bool {
            if let Some(u) = &unit
                && !line.contains(u.as_str())
            {
                return false;
            }
            if let Some(p) = &pattern
                && !line.contains(p.as_str())
            {
                return false;
            }
            if let Some(max) = prio
                && let Some(rest) = line.strip_prefix('<')
                && let Some(gt) = rest.find('>')
                && let Ok(n) = rest[..gt].parse::<u8>()
                && n > max
            {
                return false;
            }
            true
        };
        let Ok(mut f) = std::fs::File::open(&file) else {
            eprintln!("logctl: cannot open {}", file);
            return ExitCode::FAILURE;
        };
        let mut content = String::new();
        if f.read_to_string(&mut content).is_err() {
            eprintln!("logctl: cannot read {}", file);
            return ExitCode::FAILURE;
        }
        let lines: Vec<&str> = content.lines().filter(|l| matches(l)).collect();
        let start = match last_n {
            Some(n) if lines.len() > n => lines.len() - n,
            _ => 0,
        };
        let out = std::io::stdout();
        let mut out = out.lock();
        for l in &lines[start..] {
            let _ = writeln!(out, "{}", l);
        }
        let _ = out.flush();
        if !follow {
            return ExitCode::SUCCESS;
        }
        // 跟随：轮询文件新增内容
        let mut pos = f.seek(SeekFrom::End(0)).unwrap_or(0);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let Ok(meta) = f.metadata() else { break };
            if meta.len() < pos {
                pos = 0; // 轮转：从头读
            }
            if meta.len() == pos {
                continue;
            }
            let _ = f.seek(SeekFrom::Start(pos));
            let mut buf = String::new();
            if f.read_to_string(&mut buf).is_err() {
                break;
            }
            pos = f.stream_position().unwrap_or(pos);
            for l in buf.lines().filter(|l| matches(l)) {
                let _ = writeln!(out, "{}", l);
            }
            let _ = out.flush();
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(LOGCTL.name(), "logctl");
        assert!(LOGCTL.help().contains("-F"));
    }

    #[test]
    fn queries_file_with_filters() {
        let path = format!("/tmp/rbox_logctl_{}.log", std::process::id());
        std::fs::write(
            &path,
            "hello one\nunit-x started\nunit-y started\nhello two\n",
        )
        .unwrap();
        assert_eq!(
            LOGCTL.run(&[
                "-F".to_string(),
                path.clone(),
                "-u".to_string(),
                "unit-x".to_string()
            ]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            LOGCTL.run(&[
                "-F".to_string(),
                path.clone(),
                "-n".to_string(),
                "1".to_string()
            ]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            LOGCTL.run(&["-F".to_string(), path.clone(), "hello".to_string()]),
            ExitCode::SUCCESS
        );
        let _ = std::fs::remove_file(&path);
    }
}
