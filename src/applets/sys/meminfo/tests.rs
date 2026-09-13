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
fn format_processes_output() {
    let procs = vec![
        ProcMem {
            pid: 49,
            ppid: 1,
            vsz_kb: 22528,
            rss_kb: 11264,
            state: "S".to_string(),
            name: "rgetty".to_string(),
            exe: "/bin/rbox".to_string(),
            cpu_ticks: 0,
        },
        ProcMem {
            pid: 1,
            ppid: 0,
            vsz_kb: 8192,
            rss_kb: 4096,
            state: "S".to_string(),
            name: "init".to_string(),
            exe: "/bin/rbox".to_string(),
            cpu_ticks: 0,
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
fn compute_accounting_sums_to_total() {
    // 各项之和（含 Other 兜底）必须等于 MemTotal（对账恒等）
    let fields = parse_meminfo(SAMPLE_MEMINFO);
    let acc = compute_accounting(&fields);
    let sum: u64 = acc.items.iter().map(|i| i.kb).sum();
    assert_eq!(sum, acc.total_kb);
    // 分类校验：cache 已扣 Shmem，kernel 为固定内核字段之和
    assert_eq!(acc.user_kb, 663168 + 4104);
    assert_eq!(acc.kernel_kb, 0); // 样例无 SReclaimable/SUnreclaim 等
    // MemTotal = 1928844
    assert_eq!(acc.total_kb, 1928844);
}

#[test]
fn format_accounting_output() {
    let fields = parse_meminfo(SAMPLE_MEMINFO);
    let lines = format_accounting(&fields, Unit::Kb);
    assert!(lines[0].contains("Memory accounting (kB)"), "{}", lines[0]);
    let out = lines.join("\n");
    assert!(out.contains("MemFree"), "{}", out);
    assert!(out.contains("User"), "{}", out);
    assert!(out.contains("Kernel"), "{}", out);
    assert!(out.contains("== MemTotal"), "{}", out);
    assert!(out.contains("1928844"), "{}", out);
}

#[test]
fn format_accounting_columns_aligned() {
    let fields = parse_meminfo(SAMPLE_MEMINFO);
    let lines = format_accounting(&fields, Unit::Kb);
    // 所有含 '%' 的数据行（标题/分隔线除外）百分比列位置一致 → 列对齐
    let positions: Vec<usize> = lines[1..]
        .iter()
        .filter(|l| l.contains('%'))
        .map(|l| l.rfind('%').unwrap_or(0))
        .collect();
    let first = positions[0];
    assert!(
        positions.iter().all(|&p| p == first),
        "pct 列未对齐: {:?}\n{}",
        positions,
        lines.join("\n")
    );
}

#[test]
fn format_accounting_mismatch_shown() {
    // 异常数据（明细之和 > MemTotal）时显示差额提示而非恒等标记
    let fields = parse_meminfo("MemTotal: 100 kB\nMemFree: 200 kB\n");
    let lines = format_accounting(&fields, Unit::Kb);
    let out = lines.join("\n");
    assert!(out.contains("!="), "{}", out);
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
