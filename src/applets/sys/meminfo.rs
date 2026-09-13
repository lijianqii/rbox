//! `meminfo` - 查看内存使用情况与进程内存占用（读 /proc）。
//!
//! 用法：`meminfo [-bkmg] [-a]`
//! - 单位：`-b` 字节、`-k` KB（默认）、`-m` MB、`-g` GB；
//! - `-a`：进程列表显示全部（默认过滤 RSS=0 的内核线程）。
//!
//! 输出三部分：
//! 1. 总览（free 风格）：total/used/free/shared/buff/cache/available；
//!    used = total - free - buff/cache；buff/cache = Buffers + Cached；
//! 2. 详细明细（/proc/meminfo 常见字段，两列排布）+ iomem 树状映射；
//! 3. 进程列表（PID/PPID/VSZ/RSS/%MEM/STATE/COMMAND，按 RSS 降序）。
//!
//! 数据来源路径均可配置（/etc/rbox.conf [paths] proc/meminfo/iomem）。

use crate::applet::Applet;
use crate::applets::proc::ProcMem;
use crate::applets::proc::{collect_processes, sort_processes};
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

    /// 单位标签（用于明细/进程列表标题）。
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

        // 内存分类核算（各分类之和与 MemTotal 对账）
        println!();
        for line in format_accounting(&fields, unit) {
            println!("{}", line);
        }

        // 物理内存映射（/proc/iomem；文件不可读时静默输出空表）
        println!();
        for line in read_iomem() {
            println!("{}", line);
        }

        // 进程内存占用（按 RSS 降序；默认过滤 RSS=0 的内核线程，-a 显示全部）
        let mut procs = collect_processes();
        sort_processes(&mut procs);
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
    for pair in values.chunks(2) {
        let left = format!("  {:<20}{:>12}", pair[0].0, pair[0].1);
        let right = pair
            .get(1)
            .map(|(n, v)| format!("  {:<20}{:>12}", n, v))
            .unwrap_or_default();
        lines.push(format!("{}{}", left, right));
    }
    lines
}

/// 一条内存分类核算项。
pub(crate) struct AccountingItem {
    label: &'static str,
    desc: &'static str,
    kb: u64,
}

/// 内存分类核算结果：明细项 + 用户态/内核态汇总 + 总内存。
pub(crate) struct Accounting {
    pub(crate) items: Vec<AccountingItem>,
    pub(crate) user_kb: u64,
    pub(crate) kernel_kb: u64,
    pub(crate) total_kb: u64,
}

/// 把 MemTotal 拆解为互不重叠的分类（页缓存已扣除 Shmem，避免重复计数），
/// 各项之和恒等于 MemTotal（"other/unclassified"兜底），用于对账。
pub(crate) fn compute_accounting(fields: &HashMap<String, u64>) -> Accounting {
    let total = *fields.get("MemTotal").unwrap_or(&0);
    let free = *fields.get("MemFree").unwrap_or(&0);
    let anon = *fields.get("AnonPages").unwrap_or(&0);
    let shmem = *fields.get("Shmem").unwrap_or(&0);
    let buffers = *fields.get("Buffers").unwrap_or(&0);
    let reclaimable = *fields.get("SReclaimable").unwrap_or(&0);
    let kernel = *fields.get("SUnreclaim").unwrap_or(&0)
        + *fields.get("PageTables").unwrap_or(&0)
        + *fields.get("KernelStack").unwrap_or(&0)
        + *fields.get("VmallocUsed").unwrap_or(&0)
        + *fields.get("Bounce").unwrap_or(&0);
    // Cached 包含 tmpfs/共享页（Shmem），扣减后作为纯页缓存，避免与 Shmem 重复
    let cache = fields
        .get("Cached")
        .copied()
        .unwrap_or(0)
        .saturating_sub(shmem);
    let accounted = free + anon + shmem + cache + buffers + reclaimable + kernel;
    let other = total.saturating_sub(accounted);
    Accounting {
        items: vec![
            AccountingItem {
                label: "MemFree",
                desc: "free",
                kb: free,
            },
            AccountingItem {
                label: "AnonPages",
                desc: "anon pages",
                kb: anon,
            },
            AccountingItem {
                label: "Shmem",
                desc: "shared",
                kb: shmem,
            },
            AccountingItem {
                label: "Cached",
                desc: "page cache",
                kb: cache,
            },
            AccountingItem {
                label: "Buffers",
                desc: "buffers",
                kb: buffers,
            },
            AccountingItem {
                label: "SReclaimable",
                desc: "reclaimable slab",
                kb: reclaimable,
            },
            AccountingItem {
                label: "Kernel",
                desc: "kernel fixed",
                kb: kernel,
            },
            AccountingItem {
                label: "Other",
                desc: "other/unclassified",
                kb: other,
            },
        ],
        user_kb: anon + shmem,
        kernel_kb: reclaimable + kernel,
        total_kb: total,
    }
}

/// 生成内存分类核算输出（各项 + 用户态/内核态汇总 + Total 对账）。
fn format_accounting(fields: &HashMap<String, u64>, unit: Unit) -> Vec<String> {
    let acc = compute_accounting(fields);
    let total = acc.total_kb;
    let pct = |kb: u64| {
        if total > 0 {
            kb as f64 * 100.0 / total as f64
        } else {
            0.0
        }
    };
    let mut lines = Vec::new();
    lines.push(format!("Memory accounting ({}):", unit.label()));
    // desc 列宽 = 最长描述（含 User/Kernel/Total 汇总行），保证值列对齐
    let desc_w = acc
        .items
        .iter()
        .map(|i| i.desc.chars().count())
        .chain(
            ["user total", "kernel total", "total"]
                .iter()
                .map(|s| s.chars().count()),
        )
        .max()
        .unwrap_or(10);
    let row = |label: &str, desc: &str, kb: u64| {
        format!(
            "  {:<12} {:<desc_w$} {:>10} {:>6.1}%",
            label,
            desc,
            unit.convert(kb),
            pct(kb),
            desc_w = desc_w
        )
    };
    for it in &acc.items {
        lines.push(row(it.label, it.desc, it.kb));
    }
    lines.push(format!("  {}", "-".repeat(40)));
    lines.push(row("User", "user total", acc.user_kb));
    lines.push(row("Kernel", "kernel total", acc.kernel_kb));
    // 明细各项之和（含 Other）应等于 MemTotal
    let sum: u64 = acc.items.iter().map(|i| i.kb).sum();
    let check = if sum == total {
        "(== MemTotal)".to_string()
    } else {
        format!(
            "(MemTotal {} != sum {})",
            unit.convert(total),
            unit.convert(sum)
        )
    };
    lines.push(format!("{}  {}", row("Total", "total", sum), check));
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

/// 生成进程内存列表行（VSZ/RSS 单位随 unit，%MEM 为 RSS 占总内存百分比）。
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
mod tests;
