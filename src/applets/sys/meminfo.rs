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

    /// 单位标签（用于 RSS 列表标题）。
    fn label(self) -> &'static str {
        match self {
            Unit::Bytes => "B",
            Unit::Kb => "kB",
            Unit::Mb => "MB",
            Unit::Gb => "GB",
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
        "meminfo [-bkmg] [-a] - show memory usage and per-process RSS (from /proc)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut unit = Unit::Kb;
        let mut show_all = false;
        for a in args {
            match a.as_str() {
                "-b" => unit = Unit::Bytes,
                "-k" => unit = Unit::Kb,
                "-m" => unit = Unit::Mb,
                "-g" => unit = Unit::Gb,
                "-a" => show_all = true,
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
        let fields = parse_meminfo(&content);
        let stats = match compute_stats_from_map(&fields) {
            Some(s) => s,
            None => {
                eprintln!("meminfo: cannot parse {}", path);
                return ExitCode::FAILURE;
            }
        };
        for line in format_stats(&stats, unit) {
            println!("{}", line);
        }

        // 详细内存明细（/proc/meminfo 常见字段，两列排布压缩行数）
        println!();
        for line in format_detail(&fields, unit) {
            println!("{}", line);
        }

        // 物理内存映射（/proc/iomem）
        println!();
        for line in read_iomem() {
            println!("{}", line);
        }

        // 进程内存占用（按 RSS 降序；默认过滤 RSS=0 的内核线程，-a 显示全部）
        let mut procs = collect_processes();
        procs.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb).then(a.pid.cmp(&b.pid)));
        if !show_all {
            procs.retain(|p| p.rss_kb > 0);
        }
        println!();
        println!("Processes (by RSS):");
        for line in format_processes(&procs, unit, stats.total) {
            println!("{}", line);
        }
        ExitCode::SUCCESS
    }
}

/// 一个进程的内存信息（RSS 单位 kB）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcMem {
    pub(crate) pid: u32,
    pub(crate) ppid: u32,
    /// 虚拟内存大小（kB，statm size 字段）
    pub(crate) vsz_kb: u64,
    /// 常驻内存（kB，statm resident 字段）
    pub(crate) rss_kb: u64,
    /// 进程状态（stat 第 3 字段，如 S/R/Z）
    pub(crate) state: String,
    pub(crate) name: String,
}

/// 遍历 /proc 收集所有进程的内存信息（RSS 来自 statm 的 resident 页数）。
fn collect_processes() -> Vec<ProcMem> {
    let page_kb = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64) / 1024;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let (size_pages, rss_pages) = read_statm(pid);
        let ppid = read_ppid(pid);
        let state = read_state(pid);
        let comm = read_comm(pid);
        out.push(ProcMem {
            pid,
            ppid,
            vsz_kb: size_pages * page_kb,
            rss_kb: rss_pages * page_kb,
            state,
            name: comm,
        });
    }
    out
}

fn read_statm(pid: u32) -> (u64, u64) {
    let content = std::fs::read_to_string(format!("/proc/{}/statm", pid)).unwrap_or_default();
    parse_statm(&content)
}

/// 解析 statm 文本，返回 (size 页数, resident 页数)（第 1、2 个字段）。
pub(crate) fn parse_statm(content: &str) -> (u64, u64) {
    let mut it = content.split_whitespace();
    let size = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let resident = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (size, resident)
}

fn read_ppid(pid: u32) -> u32 {
    let content = std::fs::read_to_string(format!("/proc/{}/stat", pid)).unwrap_or_default();
    parse_stat_ppid(&content)
}

/// 解析 stat 文本，返回 ppid（第 4 字段；comm 可能含空格/括号，从 ")" 后取）。
pub(crate) fn parse_stat_ppid(content: &str) -> u32 {
    let Some(rest) = content.split(')').nth(1) else {
        return 0;
    };
    rest.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn read_state(pid: u32) -> String {
    let content = std::fs::read_to_string(format!("/proc/{}/stat", pid)).unwrap_or_default();
    parse_stat_state(&content)
}

/// 解析 stat 文本，返回进程状态（")" 后第 1 个字段，如 S/R/Z）。
pub(crate) fn parse_stat_state(content: &str) -> String {
    let Some(rest) = content.split(')').nth(1) else {
        return String::new();
    };
    rest.split_whitespace().next().unwrap_or("").to_string()
}

fn read_comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
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

/// 由 /proc/meminfo 内容计算统计值（仅测试使用；run 用 compute_stats_from_map）。
#[cfg(test)]
pub(crate) fn compute_stats(content: &str) -> Option<MemStats> {
    compute_stats_from_map(&parse_meminfo(content))
}

/// 由解析后的字段 map 计算统计值（run 与 compute_stats 共用，避免重复解析）。
pub(crate) fn compute_stats_from_map(m: &HashMap<String, u64>) -> Option<MemStats> {
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

/// 详细明细中展示的 /proc/meminfo 字段（按分组顺序）。
const DETAIL_FIELDS: &[&str] = &[
    // 总量/空闲/共享
    "MemTotal",
    "MemFree",
    "MemAvailable",
    "Shmem",
    // 缓存与回写
    "Buffers",
    "Cached",
    "SwapCached",
    "SReclaimable",
    "SUnreclaim",
    "Dirty",
    "Writeback",
    "WritebackTmp",
    "AnonPages",
    "Mapped",
    "PageTables",
    "KernelStack",
    "Bounce",
    // 活跃/不活跃
    "Active",
    "Inactive",
    "Active(anon)",
    "Inactive(anon)",
    "Active(file)",
    "Inactive(file)",
    "Unevictable",
    "Mlocked",
    "Slab",
    // 交换与内核虚拟内存
    "SwapTotal",
    "SwapFree",
    "Committed_AS",
    "VmallocTotal",
    "VmallocUsed",
    "VmallocChunk",
];

/// 生成详细内存明细行（两列排布，字段缺失自动跳过）。
fn format_detail(fields: &HashMap<String, u64>, unit: Unit) -> Vec<String> {
    let values: Vec<(&str, u64)> = DETAIL_FIELDS
        .iter()
        .filter_map(|n| fields.get(*n).map(|v| (*n, unit.convert(*v))))
        .collect();
    let mut lines = Vec::new();
    lines.push(format!("Memory detail ({}):", unit.label()));
    let mut i = 0;
    while i < values.len() {
        let left = format!("  {:<20}{:>12}", values[i].0, values[i].1);
        let right = if i + 1 < values.len() {
            format!("  {:<20}{:>12}", values[i + 1].0, values[i + 1].1)
        } else {
            String::new()
        };
        lines.push(format!("{}{}", left, right));
        i += 2;
    }
    lines
}

/// 读取 /proc/iomem 并生成树状内存映射输出。
fn read_iomem() -> Vec<String> {
    let path = &crate::config::load().paths.iomem;
    let content = std::fs::read_to_string(path).unwrap_or_default();
    format_iomem(&content)
}

/// 一条 iomem 映射（depth 为内核缩进层级，2 空格/级）。
pub(crate) struct IomemEntry {
    pub(crate) depth: usize,
    pub(crate) text: String,
}

/// 解析 /proc/iomem 内容为带深度的条目。
pub(crate) fn parse_iomem(content: &str) -> Vec<IomemEntry> {
    content
        .lines()
        .filter_map(|line| {
            let text = line.trim();
            if text.is_empty() {
                return None;
            }
            let leading = line.len() - line.trim_start().len();
            Some(IomemEntry {
                depth: leading / 2,
                text: text.to_string(),
            })
        })
        .collect()
}

/// 渲染树状映射（tree 风格）：父区域直显；子区域用 `├──`/`└──` 连接，
/// 祖先层级用 `│` 延续（多根之间不连线，与 tree 命令一致）。
pub(crate) fn render_iomem(entries: &[IomemEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if e.depth == 0 {
            out.push(format!("  {}", e.text));
            continue;
        }
        // 前缀：depth-1 段，第 level 段表示第 level 层祖先是否还有后续兄弟
        let mut prefix = String::new();
        for level in 1..e.depth {
            let cont = entries[i + 1..]
                .iter()
                .find(|n| n.depth <= level)
                .map(|n| n.depth == level)
                .unwrap_or(false);
            prefix.push_str(if cont { "│   " } else { "    " });
        }
        // 连接符：本节点之后是否还有同级（中间没有更浅层）
        let has_sibling = entries[i + 1..]
            .iter()
            .find(|n| n.depth <= e.depth)
            .map(|n| n.depth == e.depth)
            .unwrap_or(false);
        let conn = if has_sibling {
            "├── "
        } else {
            "└── "
        };
        out.push(format!("  {}{}{}", prefix, conn, e.text));
    }
    out
}

/// 生成内存映射输出（纯函数，便于测试）。
pub(crate) fn format_iomem(content: &str) -> Vec<String> {
    let mut lines = vec!["Memory map (/proc/iomem):".to_string()];
    lines.extend(render_iomem(&parse_iomem(content)));
    lines
}

/// 以 free 风格输出统计。
/// 生成统计输出行（纯函数，便于测试）。
/// 对齐规则：标签列固定 14 宽（左对齐），数字列 13 宽右对齐，
/// 标题行与数据行使用相同的列宽，保证各列右缘一致。
fn format_stats(stats: &MemStats, unit: Unit) -> Vec<String> {
    let c = |v: u64| format!("{:>13}", unit.convert(v));
    vec![
        format!(
            "{:<14}{:>13}{:>13}{:>13}{:>13}{:>13}{:>13}",
            "", "total", "used", "free", "shared", "buff/cache", "available"
        ),
        format!(
            "{:<14}{}{}{}{}{}{}",
            "Mem:",
            c(stats.total),
            c(stats.used),
            c(stats.free),
            c(stats.shared),
            c(stats.buff_cache),
            c(stats.available),
        ),
        format!(
            "{:<14}{}{}{}",
            "Swap:",
            c(stats.swap_total),
            c(stats.swap_used),
            c(stats.swap_free),
        ),
    ]
}

/// 生成进程内存列表行（按 RSS 降序，单位随 unit）。
/// 生成进程内存列表行（按 RSS 降序；VSZ/RSS 单位随 unit，%MEM 为 RSS 占总内存百分比）。
fn format_processes(procs: &[ProcMem], unit: Unit, mem_total_kb: u64) -> Vec<String> {
    let mut lines = Vec::with_capacity(procs.len() + 1);
    lines.push(format!(
        "{:>7}{:>7}{:>13}{:>13}{:>8} {:<5} {}",
        "PID",
        "PPID",
        format!("VSZ({})", unit.label()),
        format!("RSS({})", unit.label()),
        "%MEM",
        "STATE",
        "COMMAND"
    ));
    for p in procs {
        let pct = if mem_total_kb > 0 {
            p.rss_kb as f64 * 100.0 / mem_total_kb as f64
        } else {
            0.0
        };
        lines.push(format!(
            "{:>7}{:>7}{:>13}{:>13}{:>8.1} {:<5} {}",
            p.pid,
            p.ppid,
            unit.convert(p.vsz_kb),
            unit.convert(p.rss_kb),
            pct,
            p.state,
            p.name
        ));
    }
    lines
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
AnonPages:       663168 kB
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

    #[test]
    fn stats_columns_aligned() {
        // 标题行与数据行使用相同的标签列(14) + 数字列(13)宽度，
        // 各列右缘位置一致："total" 右缘 == Mem 行第一列数字右缘
        let s = compute_stats(SAMPLE_MEMINFO).unwrap();
        let lines = format_stats(&s, Unit::Kb);
        let title = &lines[0];
        let mem = &lines[1];
        let title_total_end = title.find("total").unwrap() + "total".len();
        let mem_first_end = mem.find("1928844").unwrap() + "1928844".len();
        assert_eq!(
            title_total_end, mem_first_end,
            "\ntitle: {}\nmem:   {}",
            title, mem
        );
    }

    #[test]
    fn parse_statm_returns_size_and_resident() {
        assert_eq!(parse_statm("100 45 2 1 0 0 0\n"), (100, 45));
        assert_eq!(parse_statm("100 0 0 0 0 0 0\n"), (100, 0));
        assert_eq!(parse_statm(""), (0, 0));
        assert_eq!(parse_statm("not a statm"), (0, 0));
    }

    #[test]
    fn parse_stat_ppid_handles_spaces_in_comm() {
        // comm 含空格/括号：pid (my proc) S 1 2 3 ...，ppid 是 ") " 后的第 2 个字段
        assert_eq!(parse_stat_ppid("123 (my proc) S 1 2 3 4\n"), 1);
        assert_eq!(parse_stat_ppid("1 (rbox) S 0 1 1\n"), 0);
        assert_eq!(parse_stat_ppid("no parens\n"), 0);
    }

    #[test]
    fn parse_stat_state_field() {
        assert_eq!(parse_stat_state("123 (my proc) S 1 2 3 4\n"), "S");
        assert_eq!(parse_stat_state("1 (rbox) R 0 1 1\n"), "R");
        assert_eq!(parse_stat_state("no parens\n"), "");
    }

    #[test]
    fn format_processes_output() {
        let procs = vec![
            ProcMem {
                pid: 49,
                ppid: 1,
                vsz_kb: 22528,
                rss_kb: 11264,
                state: "S".to_string(),
                name: "rgetty".to_string(),
            },
            ProcMem {
                pid: 1,
                ppid: 0,
                vsz_kb: 8192,
                rss_kb: 4096,
                state: "S".to_string(),
                name: "init".to_string(),
            },
        ];
        let lines = format_processes(&procs, Unit::Kb, 91768);
        assert!(lines[0].contains("PID"), "out: {}", lines[0]);
        assert!(lines[0].contains("VSZ(kB)"), "out: {}", lines[0]);
        assert!(lines[0].contains("RSS(kB)"), "out: {}", lines[0]);
        assert!(lines[0].contains("%MEM"), "out: {}", lines[0]);
        assert!(lines[1].contains("rgetty"), "out: {}", lines[1]);
        assert!(lines[1].contains("22528"), "out: {}", lines[1]);
        // %MEM = 11264 / 91768 * 100 = 12.3
        assert!(lines[1].contains("12.3"), "out: {}", lines[1]);
        // MB 单位：标题 VSZ(MB)，值 22528/1024 = 22
        let lines_m = format_processes(&procs, Unit::Mb, 91768);
        assert!(lines_m[0].contains("VSZ(MB)"), "out: {}", lines_m[0]);
        assert!(lines_m[1].contains("22"), "out: {}", lines_m[1]);
        // mem_total 为 0 时 %MEM 显示 0.0
        let lines_zero = format_processes(&procs, Unit::Kb, 0);
        assert!(lines_zero[1].contains("0.0"), "out: {}", lines_zero[1]);
    }

    #[test]
    fn format_detail_lists_fields() {
        let fields = parse_meminfo(SAMPLE_MEMINFO);
        let lines = format_detail(&fields, Unit::Kb);
        assert!(lines[0].contains("Memory detail (kB)"), "out: {}", lines[0]);
        let out = lines.join("\n");
        assert!(out.contains("MemTotal"), "out: {}", out);
        assert!(out.contains("AnonPages"), "out: {}", out);
        assert!(out.contains("SwapTotal"), "out: {}", out);
        // 两列排布：样例含 18 个字段 → 9 行 + 标题 = 10 行
        assert_eq!(lines.len(), 10, "out: {}", out);
        // 值跟随单位换算：MemTotal 1928844 / 1024 = 1883 (MB)
        let lines_m = format_detail(&fields, Unit::Mb);
        let memtotal_line = lines_m
            .iter()
            .find(|l| l.contains("MemTotal"))
            .unwrap()
            .to_string();
        assert!(memtotal_line.contains("1883"), "out: {}", memtotal_line);
    }

    #[test]
    fn format_detail_skips_missing_fields() {
        let fields = parse_meminfo("MemTotal: 100 kB\nMemFree: 50 kB\n");
        let lines = format_detail(&fields, Unit::Kb);
        let out = lines.join("\n");
        assert!(out.contains("MemTotal"), "out: {}", out);
        assert!(!out.contains("AnonPages"), "out: {}", out);
    }

    #[test]
    fn format_iomem_tree_shape() {
        // 两级：每个顶层区域一个子区域 → 子区域用 └──
        let content = "00000000-03ffffff : 0.flash flash@0\n09000000-09000fff : pl011@9000000\n  09000000-09000fff : 9000000.pl011 pl011@9000000\n09010000-09010fff : pl031@9010000\n  09010000-09010fff : rtc-pl031\n";
        let lines = format_iomem(content);
        assert_eq!(lines[0], "Memory map (/proc/iomem):");
        assert_eq!(lines[1], "  00000000-03ffffff : 0.flash flash@0");
        assert_eq!(
            lines[3],
            "  └── 09000000-09000fff : 9000000.pl011 pl011@9000000"
        );
        assert_eq!(lines[5], "  └── 09010000-09010fff : rtc-pl031");
    }

    #[test]
    fn render_iomem_siblings() {
        // 多子：非最后子用 ├──，最后子用 └──
        let entries = parse_iomem("parent\n  child1\n  child2\n");
        let lines = render_iomem(&entries);
        assert_eq!(lines[0], "  parent");
        assert_eq!(lines[1], "  ├── child1");
        assert_eq!(lines[2], "  └── child2");
    }

    #[test]
    fn render_iomem_deep_prefix() {
        // 三级：a -> b -> c，b 非最后子（后面还有 d），c 是最后子
        let entries = parse_iomem("a\n  b\n    c\n  d\n");
        let lines = render_iomem(&entries);
        assert_eq!(lines[0], "  a");
        // b 是 a 的非最后子
        assert_eq!(lines[1], "  ├── b");
        // c 是 b 的唯一子：层 1 祖先 b 还有后续 → 前缀 │，连接符 └──
        assert_eq!(lines[2], "  │   └── c");
        assert_eq!(lines[3], "  └── d");
    }

    #[test]
    fn render_iomem_no_line_between_roots() {
        // 多个顶层区域之间不画延续线（tree 多根语义）
        let entries = parse_iomem("root1\n  child\nroot2\n");
        let lines = render_iomem(&entries);
        assert_eq!(lines[0], "  root1");
        assert_eq!(lines[1], "  └── child");
        assert_eq!(lines[2], "  root2");
    }

    #[test]
    fn format_iomem_empty() {
        let lines = format_iomem("");
        assert_eq!(lines, vec!["Memory map (/proc/iomem):".to_string()]);
    }
}
