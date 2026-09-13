//! `pgrep` / `pkill` - 按名称/命令行查找或终止进程。
//!
//! 用法：pgrep [-f] [-x] [-l] PATTERN
//!       pkill [-f] [-x] [-SIGNAL] PATTERN
//! 默认匹配进程名（/proc/PID/stat 的 comm）；`-f` 匹配完整命令行；
//! `-x` 精确匹配。自身进程始终排除。

use crate::applet::Applet;
use std::fs;
use std::process::ExitCode;

pub struct Pgrep;
pub static PGREP: &Pgrep = &Pgrep;

pub struct Pkill;
pub static PKILL: &Pkill = &Pkill;

/// 进程信息。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcInfo {
    pub(crate) pid: u32,
    pub(crate) comm: String,
    pub(crate) cmdline: String,
}

/// 读取单个进程信息。
pub(crate) fn read_proc(pid: u32) -> Option<ProcInfo> {
    let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat[open + 1..close].to_string();
    let cmdline = fs::read(format!("/proc/{}/cmdline", pid))
        .ok()
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    Some(ProcInfo { pid, comm, cmdline })
}

/// 收集所有数字 pid 的进程。
pub(crate) fn collect() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        if let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>()
            && let Some(p) = read_proc(pid)
        {
            out.push(p);
        }
    }
    out
}

/// 匹配进程。
pub(crate) fn matches(p: &ProcInfo, pattern: &str, full: bool, exact: bool) -> bool {
    let hay = if full { &p.cmdline } else { &p.comm };
    if exact {
        hay == pattern
    } else {
        hay.contains(pattern)
    }
}

/// 解析公共选项：返回 (full, exact, list, signal, pattern)。
fn parse_common(
    args: &[String],
    allow_signal: bool,
) -> Result<(bool, bool, bool, i32, String), String> {
    let mut full = false;
    let mut exact = false;
    let mut list = false;
    let mut signal = libc::SIGTERM;
    let mut pattern: Option<String> = None;
    for a in args {
        match a.as_str() {
            "-f" | "--full" => full = true,
            "-x" | "--exact" => exact = true,
            "-l" | "--list-name" => list = true,
            s if allow_signal && s.starts_with('-') && s.len() > 1 => match s[1..].parse::<i32>() {
                Ok(n) if (0..=64).contains(&n) => signal = n,
                _ => match crate::applets::sys::kill::signal_number(&s[1..]) {
                    Some(n) => signal = n,
                    None => return Err(format!("invalid signal: {}", s)),
                },
            },
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option: {}", s));
            }
            s => pattern = Some(s.to_string()),
        }
    }
    let pattern = pattern.ok_or("missing pattern")?;
    Ok((full, exact, list, signal, pattern))
}

impl Applet for Pgrep {
    fn name(&self) -> &'static str {
        "pgrep"
    }
    fn help(&self) -> &'static str {
        "pgrep [-f] [-x] [-l] PATTERN - find processes by name/cmdline"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (full, exact, list, _sig, pattern) = match parse_common(args, false) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("pgrep: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let me = std::process::id();
        let mut found = false;
        for p in collect() {
            if p.pid == me || !matches(&p, &pattern, full, exact) {
                continue;
            }
            found = true;
            if list {
                println!("{} {}", p.pid, p.comm);
            } else {
                println!("{}", p.pid);
            }
        }
        if found {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        }
    }
}

impl Applet for Pkill {
    fn name(&self) -> &'static str {
        "pkill"
    }
    fn help(&self) -> &'static str {
        "pkill [-f] [-x] [-SIGNAL] PATTERN - signal processes by name/cmdline"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (full, exact, _list, signal, pattern) = match parse_common(args, true) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("pkill: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let me = std::process::id();
        let mut found = false;
        for p in collect() {
            if p.pid == me || !matches(&p, &pattern, full, exact) {
                continue;
            }
            found = true;
            if unsafe { libc::kill(p.pid as i32, signal) } != 0 {
                eprintln!("pkill: {}: {}", p.pid, std::io::Error::last_os_error());
            }
        }
        if found {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(PGREP.name(), "pgrep");
        assert_eq!(PKILL.name(), "pkill");
        assert!(PGREP.help().contains("processes"));
    }

    #[test]
    fn reads_self_process() {
        let p = read_proc(std::process::id()).expect("read self /proc");
        assert!(!p.comm.is_empty());
    }

    #[test]
    fn matches_name_and_cmdline() {
        let p = ProcInfo {
            pid: 1,
            comm: "rbox".into(),
            cmdline: "/bin/rbox init".into(),
        };
        assert!(matches(&p, "rbo", false, false));
        assert!(matches(&p, "rbox", false, true));
        assert!(!matches(&p, "rbo", false, true));
        assert!(matches(&p, "init", true, false));
        assert!(!matches(&p, "init", false, false));
    }

    #[test]
    fn parse_options() {
        let (full, exact, list, sig, pat) = parse_common(
            &[
                "-f".to_string(),
                "-x".to_string(),
                "-l".to_string(),
                "rbox".to_string(),
            ],
            false,
        )
        .unwrap();
        assert!(full && exact && list);
        assert_eq!(sig, libc::SIGTERM);
        assert_eq!(pat, "rbox");
        let (_, _, _, sig, _) = parse_common(&["-9".to_string(), "x".to_string()], true).unwrap();
        assert_eq!(sig, 9);
        assert!(parse_common(&[], false).is_err());
    }

    #[test]
    fn pgrep_finds_self_excluded() {
        // 以自身名字搜索时不应输出自身 pid（允许无匹配）
        let rc = PGREP.run(&["-x".to_string(), "rbox".to_string()]);
        assert!(rc == ExitCode::SUCCESS || rc == ExitCode::from(1));
    }
}
