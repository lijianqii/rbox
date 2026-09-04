//! `processes` - 以树形结构显示所有进程（btop 风格）。
//!
//! 用法：`processes`
//!
//! 所有进程挂在虚拟根 `system` 下（init、kthreadd、孤儿进程作为其分支），
//! 每行显示 PID / 名称 / 状态 / RSS（自适应单位）/ %MEM；子进程按 PID 升序，
//! 连接符与 btop/tree 一致（`├──`/`└──`/`│`）。数据源路径可配置（[paths] proc）。

use crate::applet::Applet;
use crate::applets::sys::proc::{ProcMem, collect_processes, mem_total_kb};
use std::collections::HashMap;
use std::process::ExitCode;

pub struct Processes;
pub static PROCESSES: &Processes = &Processes;

/// 进程树节点（虚拟根 pid 为 0，名称为 system）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcTree {
    pub(crate) pid: u32,
    pub(crate) ppid: u32,
    pub(crate) name: String,
    /// 进程状态（stat 第 3 字段，如 S/R/Z）
    pub(crate) state: String,
    /// 常驻内存（kB）
    pub(crate) rss_kb: u64,
    /// 虚拟内存（kB）
    pub(crate) vsz_kb: u64,
    pub(crate) children: Vec<ProcTree>,
}

impl Applet for Processes {
    fn name(&self) -> &'static str {
        "processes"
    }
    fn help(&self) -> &'static str {
        "processes - show all processes as a tree (btop style)"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let procs = collect_processes();
        let tree = build_process_tree(&procs);
        let mem_total = mem_total_kb();
        for line in render_process_tree(&tree, mem_total) {
            println!("{}", line);
        }
        ExitCode::SUCCESS
    }
}

/// 由进程列表构建进程树：虚拟根 `system`（pid 0）下挂所有"真根"
/// （ppid 为 0 或父进程不存在），使 init/kthreadd/孤儿进程在同一分组内。
pub(crate) fn build_process_tree(procs: &[ProcMem]) -> ProcTree {
    let by_pid: HashMap<u32, &ProcMem> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut roots: Vec<&ProcMem> = procs
        .iter()
        .filter(|p| p.ppid == 0 || !by_pid.contains_key(&p.ppid))
        .collect();
    roots.sort_by_key(|p| p.pid);
    ProcTree {
        pid: 0,
        ppid: 0,
        name: "system".to_string(),
        state: String::new(),
        rss_kb: 0,
        vsz_kb: 0,
        children: roots.iter().map(|r| build_node(r, procs)).collect(),
    }
}

/// 递归构建一个进程节点及其子树（子进程按 PID 升序）。
fn build_node(p: &ProcMem, procs: &[ProcMem]) -> ProcTree {
    let mut children: Vec<&ProcMem> = procs.iter().filter(|c| c.ppid == p.pid).collect();
    children.sort_by_key(|c| c.pid);
    ProcTree {
        pid: p.pid,
        ppid: p.ppid,
        name: p.name.clone(),
        state: p.state.clone(),
        rss_kb: p.rss_kb,
        vsz_kb: p.vsz_kb,
        children: children.iter().map(|c| build_node(c, procs)).collect(),
    }
}

/// 自适应内存大小格式化：kB / MB / GB（一位小数）。
fn format_size(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.1}GB", kb as f64 / (1024.0 * 1024.0))
    } else if kb >= 1024 {
        format!("{:.1}MB", kb as f64 / 1024.0)
    } else {
        format!("{}kB", kb)
    }
}

/// 名称超宽时截断并追加省略号（按字符计数）。
fn truncate_name(name: &str, width: usize) -> String {
    let count = name.chars().count();
    if count <= width {
        return name.to_string();
    }
    let mut s: String = name.chars().take(width.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// 节点行内容（不含树缩进）：`PID 名称 状态 RSS %MEM`，名称列宽动态。
fn node_fields(node: &ProcTree, mem_total_kb: u64, name_width: usize) -> String {
    let pct = if mem_total_kb > 0 {
        node.rss_kb as f64 * 100.0 / mem_total_kb as f64
    } else {
        0.0
    };
    format!(
        "{:>5}  {:<width$} {:<2} {:>8} {:>6.1}%",
        node.pid,
        truncate_name(&node.name, name_width),
        node.state,
        format_size(node.rss_kb),
        pct,
        width = name_width
    )
}

/// 计算整棵树中名称的最大显示宽度（至少 6，容纳 "system"）。
fn max_name_width(root: &ProcTree) -> usize {
    fn walk(node: &ProcTree, max: &mut usize) {
        *max = (*max).max(node.name.chars().count());
        for c in &node.children {
            walk(c, max);
        }
    }
    let mut m = 6;
    walk(root, &mut m);
    m
}

/// 渲染进程树（btop/tree 风格）：虚拟根直显，子进程 ├──/└──，祖先用 │ 延续。
/// 名称列宽 = 树中最长名称（限制在 6..=20，超长名称截断）。
pub(crate) fn render_process_tree(root: &ProcTree, mem_total_kb: u64) -> Vec<String> {
    let name_width = max_name_width(root).clamp(6, 20);
    let mut out = Vec::new();
    out.push(node_fields(root, mem_total_kb, name_width));
    render_children(root, "", mem_total_kb, name_width, &mut out);
    out
}

/// 递归渲染节点的子进程。
fn render_children(
    node: &ProcTree,
    prefix: &str,
    mem_total_kb: u64,
    name_width: usize,
    out: &mut Vec<String>,
) {
    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let last = i == n - 1;
        let conn = if last { "└── " } else { "├── " };
        out.push(format!(
            "{}{}{}",
            prefix,
            conn,
            node_fields(child, mem_total_kb, name_width)
        ));
        let child_prefix = format!("{}{}", prefix, if last { "    " } else { "│   " });
        render_children(child, &child_prefix, mem_total_kb, name_width, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, name: &str) -> ProcMem {
        ProcMem {
            pid,
            ppid,
            vsz_kb: 0,
            rss_kb: 0,
            state: "S".to_string(),
            name: name.to_string(),
        }
    }

    fn proc_rss(pid: u32, ppid: u32, name: &str, rss_kb: u64, state: &str) -> ProcMem {
        ProcMem {
            pid,
            ppid,
            vsz_kb: rss_kb * 2,
            rss_kb,
            state: state.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn name_and_help() {
        assert_eq!(PROCESSES.name(), "processes");
        assert!(PROCESSES.help().contains("tree"));
    }

    #[test]
    fn build_tree_single_virtual_root() {
        // init 和 kthreadd 都挂在虚拟根 system 下（同一大分组）
        let procs = vec![
            proc(1, 0, "init"),
            proc(2, 0, "kthreadd"),
            proc(3, 2, "kworker"),
            proc(49, 1, "rgetty"),
            proc(50, 49, "sh"),
        ];
        let root = build_process_tree(&procs);
        assert_eq!(root.pid, 0);
        assert_eq!(root.name, "system");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].pid, 1); // init
        assert_eq!(root.children[1].pid, 2); // kthreadd
        // init 的子：rgetty；rgetty 的子：sh
        assert_eq!(root.children[0].children[0].pid, 49);
        assert_eq!(root.children[0].children[0].children[0].pid, 50);
        assert_eq!(root.children[1].children[0].pid, 3);
    }

    #[test]
    fn build_tree_orphan_under_virtual_root() {
        let procs = vec![proc(1, 0, "init"), proc(42, 99, "orphan")];
        let root = build_process_tree(&procs);
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].pid, 42);
    }

    #[test]
    fn build_tree_children_sorted_by_pid() {
        let procs = vec![proc(1, 0, "init"), proc(5, 1, "b"), proc(3, 1, "a")];
        let root = build_process_tree(&procs);
        assert_eq!(root.children[0].children[0].pid, 3);
        assert_eq!(root.children[0].children[1].pid, 5);
    }

    #[test]
    fn format_size_adaptive() {
        assert_eq!(format_size(512), "512kB");
        assert_eq!(format_size(1024), "1.0MB");
        assert_eq!(format_size(2528), "2.5MB");
        assert_eq!(format_size(1024 * 1024), "1.0GB");
    }

    #[test]
    fn truncate_name_works() {
        assert_eq!(truncate_name("short", 20), "short");
        assert_eq!(
            truncate_name("kworker/R-kvfree_rcu_reclaim", 20)
                .chars()
                .count(),
            20
        );
        assert!(truncate_name("kworker/R-kvfree_rcu_reclaim", 20).ends_with('…'));
        assert_eq!(truncate_name("abcdef", 6), "abcdef");
    }

    #[test]
    fn node_fields_columns() {
        let node = ProcTree {
            pid: 49,
            ppid: 1,
            name: "rgetty".to_string(),
            state: "S".to_string(),
            rss_kb: 2528,
            vsz_kb: 3908,
            children: Vec::new(),
        };
        let line = node_fields(&node, 91768, 10);
        assert!(line.contains("49"), "out: {}", line);
        assert!(line.contains("rgetty"), "out: {}", line);
        assert!(line.contains("S"), "out: {}", line);
        assert!(line.contains("2.5MB"), "out: {}", line);
        // %MEM = 2528 / 91768 * 100 = 2.8
        assert!(line.contains("2.8%"), "out: {}", line);
    }

    #[test]
    fn render_tree_btop_shape() {
        let procs = vec![
            proc_rss(1, 0, "init", 2400, "S"),
            proc_rss(2, 0, "kthreadd", 0, "S"),
            proc_rss(3, 2, "kworker", 0, "S"),
            proc_rss(49, 1, "rgetty", 2528, "S"),
            proc_rss(50, 49, "sh", 2508, "S"),
            proc_rss(51, 1, "shell2", 2000, "S"),
        ];
        let root = build_process_tree(&procs);
        let lines = render_process_tree(&root, 91768);
        assert!(
            lines[0].trim_start().starts_with("0  system"),
            "{}",
            lines[0]
        );
        assert!(lines[1].starts_with("├── "), "{}", lines[1]);
        assert!(lines[1].contains("init"), "{}", lines[1]);
        assert!(lines[2].starts_with("│   ├── "), "{}", lines[2]);
        assert!(lines[2].contains("rgetty"), "{}", lines[2]);
        assert!(lines[3].starts_with("│   │   └── "), "{}", lines[3]);
        assert!(lines[3].contains("sh"), "{}", lines[3]);
        assert!(lines[4].starts_with("│   └── "), "{}", lines[4]);
        assert!(lines[4].contains("shell2"), "{}", lines[4]);
        // kthreadd 在 system 分组内
        let kthreadd = lines.iter().find(|l| l.contains("kthreadd")).unwrap();
        assert!(kthreadd.starts_with("└── "), "{}", kthreadd);
        let kworker = lines.iter().find(|l| l.contains("kworker")).unwrap();
        assert!(kworker.contains("└── "), "{}", kworker);
    }

    #[test]
    fn render_tree_empty() {
        let root = build_process_tree(&[]);
        let lines = render_process_tree(&root, 0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("0  system"), "{}", lines[0]);
    }
}
