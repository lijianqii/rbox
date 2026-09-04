//! `meminfo` - 查看内存使用情况（类似 free，读取 /proc/meminfo）。
//!
//! 用法：`meminfo [-b] [-k] [-m] [-g]`
//! - 默认以 KB 显示；`-b` 字节、`-k` KB、`-m` MB、`-g` GB。
//!
//! 输出列：total / used / free / shared / buff/cache / available。
//! used = total - free - buff/cache（与 free 一致）；buff/cache = Buffers + Cached。

use crate::applet::Applet;
use std::collections::HashMap;
use std::process::ExitCode;

pub struct Meminfo;
pub static MEMINFO: &Meminfo = &Meminfo;

/// 显示单位。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Unit {
    Bytes,
    Kb,
    Mb,
    Gb,
}

impl Unit {
    /// 把 /proc/meminfo 的 kB 值换算为目标单位。
    fn convert(self, kb: u64) -> u64 {
        match self {
            Unit::Bytes => kb * 1024,
            Unit::Kb => kb,
            Unit::Mb => kb / 1024,
            Unit::Gb => kb / 1024 / 1024,
        }
    }
}

/// 解析后的内存统计（单位 kB）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MemStats {
    pub(crate) total: u64,
    pub(crate) used: u64,
    pub(crate) free: u64,
    pub(crate) shared: u64,
    pub(crate) buff_cache: u64,
    pub(crate) available: u64,
    pub(crate) swap_total: u64,
    pub(crate) swap_used: u64,
    pub(crate) swap_free: u64,
}

impl Applet for Meminfo {
    fn name(&self) -> &'static str {
        "meminfo"
    }
    fn help(&self) -> &'static str {
        "meminfo [-bkmg] - show memory usage (from /proc/meminfo)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut unit = Unit::Kb;
        for a in args {
            match a.as_str() {
                "-b" => unit = Unit::Bytes,
                "-k" => unit = Unit::Kb,
                "-m" => unit = Unit::Mb,
                "-g" => unit = Unit::Gb,
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("meminfo: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                _ => {} // 忽略位置参数（如文件参数，保持简单）
            }
        }

        let path = &crate::config::load().paths.meminfo;
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("meminfo: cannot read {}: {}", path, e);
                return ExitCode::FAILURE;
            }
        };
        let stats = match compute_stats(&content) {
            Some(s) => s,
            None => {
                eprintln!("meminfo: cannot parse {}", path);
                return ExitCode::FAILURE;
            }
        };
        print_stats(&stats, unit);
        ExitCode::SUCCESS
    }
}

/// 解析 /proc/meminfo 内容为字段 map（单位 kB，忽略非数字字段）。
fn parse_meminfo(content: &str) -> HashMap<String, u64> {
    content
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            let num: u64 = v.split_whitespace().next()?.parse().ok()?;
            Some((k.to_string(), num))
        })
        .collect()
}

/// 由 /proc/meminfo 内容计算统计值；关键字段缺失时返回 None。
pub(crate) fn compute_stats(content: &str) -> Option<MemStats> {
    let m = parse_meminfo(content);
    let total = *m.get("MemTotal")?;
    let free = *m.get("MemFree")?;
    let shared = *m.get("Shmem").unwrap_or(&0);
    let buffers = *m.get("Buffers").unwrap_or(&0);
    let cached = *m.get("Cached").unwrap_or(&0);
    let swap_total = *m.get("SwapTotal").unwrap_or(&0);
    let swap_free = *m.get("SwapFree").unwrap_or(&0);
    let buff_cache = buffers.saturating_add(cached);
    // used = total - free - buff/cache（与 free 一致，防下溢）
    let used = total.saturating_sub(free).saturating_sub(buff_cache);
    // available：优先 MemAvailable（缺失时用估算 total - used）
    let available = m
        .get("MemAvailable")
        .copied()
        .unwrap_or_else(|| total.saturating_sub(used));
    Some(MemStats {
        total,
        used,
        free,
        shared,
        buff_cache,
        available,
        swap_total,
        swap_used: swap_total.saturating_sub(swap_free),
        swap_free,
    })
}

/// 以 free 风格输出统计。
fn print_stats(stats: &MemStats, unit: Unit) {
    for line in format_stats(stats, unit) {
        println!("{}", line);
    }
}

/// 生成统计输出行（纯函数，便于测试）。
fn format_stats(stats: &MemStats, unit: Unit) -> Vec<String> {
    vec![
        "              total        used        free      shared  buff/cache   available"
            .to_string(),
        format!(
            "Mem:{:>12}{:>12}{:>12}{:>12}{:>12}{:>12}",
            unit.convert(stats.total),
            unit.convert(stats.used),
            unit.convert(stats.free),
            unit.convert(stats.shared),
            unit.convert(stats.buff_cache),
            unit.convert(stats.available),
        ),
        format!(
            "Swap:{:>12}{:>12}{:>12}",
            unit.convert(stats.swap_total),
            unit.convert(stats.swap_used),
            unit.convert(stats.swap_free),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MEMINFO: &str = "\
MemTotal:       1928844 kB
MemFree:         289136 kB
MemAvailable:    944524 kB
Buffers:          30452 kB
Cached:          597820 kB
SwapCached:            0 kB
Active:          763104 kB
Inactive:        445292 kB
Active(anon):    663168 kB
Inactive(anon):   84220 kB
Active(file):     99936 kB
Inactive(file):  361072 kB
Unevictable:           0 kB
Mlocked:               0 kB
SwapTotal:             0 kB
SwapFree:              0 kB
Shmem:              4104 kB
";

    #[test]
    fn name_and_help() {
        assert_eq!(MEMINFO.name(), "meminfo");
        assert!(MEMINFO.help().contains("memory"));
    }

    #[test]
    fn parse_meminfo_fields() {
        let m = parse_meminfo(SAMPLE_MEMINFO);
        assert_eq!(m.get("MemTotal"), Some(&1928844));
        assert_eq!(m.get("MemFree"), Some(&289136));
        assert_eq!(m.get("Shmem"), Some(&4104));
        assert_eq!(m.get("SwapTotal"), Some(&0));
        assert_eq!(m.get("Nonexistent"), None);
    }

    #[test]
    fn compute_stats_values() {
        let s = compute_stats(SAMPLE_MEMINFO).unwrap();
        assert_eq!(s.total, 1928844);
        assert_eq!(s.free, 289136);
        assert_eq!(s.shared, 4104);
        // buff/cache = 30452 + 597820
        assert_eq!(s.buff_cache, 628272);
        // used = 1928844 - 289136 - 628272
        assert_eq!(s.used, 1011436);
        assert_eq!(s.available, 944524);
        assert_eq!(s.swap_total, 0);
        assert_eq!(s.swap_free, 0);
        assert_eq!(s.swap_used, 0);
    }

    #[test]
    fn compute_stats_missing_key_returns_none() {
        assert!(compute_stats("MemFree: 1 kB\n").is_none());
    }

    #[test]
    fn compute_stats_available_fallback() {
        // 无 MemAvailable 时用 total - used 估算
        let content = "MemTotal: 100 kB\nMemFree: 30 kB\nBuffers: 10 kB\nCached: 20 kB\n";
        let s = compute_stats(content).unwrap();
        // used = 100 - 30 - 30 = 40；available 估算 = 100 - 40 = 60
        assert_eq!(s.available, 60);
    }

    #[test]
    fn unit_conversion() {
        assert_eq!(Unit::Kb.convert(1024), 1024);
        assert_eq!(Unit::Bytes.convert(1), 1024);
        assert_eq!(Unit::Mb.convert(1024), 1);
        assert_eq!(Unit::Gb.convert(1024 * 1024), 1);
    }

    #[test]
    fn print_stats_output() {
        let s = compute_stats(SAMPLE_MEMINFO).unwrap();
        let lines = format_stats(&s, Unit::Kb);
        let out = lines.join("\n");
        assert!(out.contains("Mem:"), "out: {}", out);
        assert!(out.contains("Swap:"), "out: {}", out);
        assert!(out.contains("1928844"), "out: {}", out);
        assert!(out.contains("944524"), "out: {}", out);
        // MB 换算：1928844 / 1024 = 1883
        let lines_m = format_stats(&s, Unit::Mb);
        assert!(lines_m[1].contains("1883"), "out: {}", lines_m[1]);
    }
}
