//! `df` - 显示文件系统磁盘使用。
//!
//! 用法：df [-h]
//! 数据来源 /proc/mounts + statfs(2)；`-h` 人类可读。

use crate::applet::Applet;
use crate::applets::proc::human_size;
use std::ffi::CString;
use std::process::ExitCode;

pub struct Df;
pub static DF: &Df = &Df;

/// 一个挂载点的使用信息。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DfEntry {
    pub(crate) device: String,
    pub(crate) mountpoint: String,
    pub(crate) total_kb: u64,
    pub(crate) used_kb: u64,
    pub(crate) avail_kb: u64,
}

/// 解析 /proc/mounts 行，返回 (device, mountpoint)。
pub(crate) fn parse_mounts(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let dev = f.next()?;
            let mp = f.next()?;
            Some((dev.to_string(), mp.to_string()))
        })
        .collect()
}

/// 查询挂载点空间。
pub(crate) fn statfs_entry(device: &str, mountpoint: &str) -> Option<DfEntry> {
    let c = CString::new(mountpoint).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bsize = st.f_bsize as u64;
    let total_kb = (st.f_blocks as u64).saturating_mul(bsize) / 1024;
    let avail_kb = (st.f_bavail as u64).saturating_mul(bsize) / 1024;
    let free_kb = (st.f_bfree as u64).saturating_mul(bsize) / 1024;
    Some(DfEntry {
        device: device.to_string(),
        mountpoint: mountpoint.to_string(),
        total_kb,
        used_kb: total_kb.saturating_sub(free_kb),
        avail_kb,
    })
}

/// 渲染一行（-h 人类可读）。
pub(crate) fn format_entry(e: &DfEntry, human: bool) -> String {
    let pct = if e.total_kb == 0 {
        0
    } else {
        (e.used_kb * 100).div_ceil(e.total_kb)
    };
    if human {
        format!(
            "{:<20} {:>8} {:>8} {:>8} {:>3}% {}",
            e.device,
            human_size(e.total_kb * 1024),
            human_size(e.used_kb * 1024),
            human_size(e.avail_kb * 1024),
            pct,
            e.mountpoint
        )
    } else {
        format!(
            "{:<20} {:>10} {:>10} {:>10} {:>3}% {}",
            e.device, e.total_kb, e.used_kb, e.avail_kb, pct, e.mountpoint
        )
    }
}

impl Applet for Df {
    fn name(&self) -> &'static str {
        "df"
    }
    fn help(&self) -> &'static str {
        "df [-h] - show filesystem disk space usage"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut human = false;
        for a in args {
            match a.as_str() {
                "-h" | "--human-readable" => human = true,
                "-k" => {}
                s if s.starts_with('-') => {
                    eprintln!("df: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                _ => {}
            }
        }
        let path = format!("{}/mounts", crate::config::load().paths.proc);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("df: cannot read {}: {}", path, e);
                return ExitCode::FAILURE;
            }
        };
        if human {
            println!(
                "{:<20} {:>8} {:>8} {:>8} {:>4} Mounted on",
                "Filesystem", "Size", "Used", "Avail", "Use%"
            );
        } else {
            println!(
                "{:<20} {:>10} {:>10} {:>10} {:>4} Mounted on",
                "Filesystem", "1K-blocks", "Used", "Available", "Use%"
            );
        }
        for (dev, mp) in parse_mounts(&content) {
            if let Some(e) = statfs_entry(&dev, &mp) {
                println!("{}", format_entry(&e, human));
            }
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(DF.name(), "df");
        assert!(DF.help().contains("disk"));
    }

    #[test]
    fn parse_mounts_lines() {
        let out = parse_mounts("proc /proc proc rw 0 0\n/dev/vda / ext4 rw 0 0\n");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ("proc".into(), "/proc".into()));
        assert_eq!(out[1].0, "/dev/vda");
    }

    #[test]
    fn statfs_root_works() {
        let e = statfs_entry("rootfs", "/").expect("statfs /");
        assert!(e.total_kb > 0);
        assert!(e.avail_kb <= e.total_kb);
    }

    #[test]
    fn format_contains_mountpoint() {
        let e = DfEntry {
            device: "/dev/vda".into(),
            mountpoint: "/".into(),
            total_kb: 1024,
            used_kb: 512,
            avail_kb: 512,
        };
        let line = format_entry(&e, false);
        assert!(line.contains("/dev/vda"));
        assert!(line.contains("50%"));
    }
}
