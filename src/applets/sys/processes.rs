//! `processes` - 以树状显示所有进程及其父子关系（类似 pstree）。
//!
//! 用法：`processes`
//!
//! 输出：每行一个进程，格式 `comm(pid)`；父进程下用 `├──`/`└──` 连接子进程，
//! 祖先层级用 `│` 延续（tree 风格）。根为 ppid 为 0 或父进程已不存在的进程，
//! 根之间不连线；子进程按 PID 升序。数据源路径可配置（[paths] proc）。

use crate::applet::Applet;
use crate::applets::sys::proc::{ProcMem, collect_processes};
use std::collections::HashMap;
use std::process::ExitCode;

pub struct Processes;
pub static PROCESSES: &Processes = &Processes;

/// 进程树节点。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcTree {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) children: Vec<ProcTree>,
}

impl Applet for Processes {
    fn name(&self) -> &'static str {
        "processes"
    }
    fn help(&self) -> &'static str {
        "processes - show all processes as a tree (like pstree)"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let procs = collect_processes();
        let roots = build_process_tree(&procs);
        for line in render_process_tree(&roots) {
            println!("{}", line);
        }
        ExitCode::SUCCESS
    }
}

/// 由进程列表构建进程树，返回根节点（ppid 为 0 或父进程不存在；按 PID 升序）。
pub(crate) fn build_process_tree(procs: &[ProcMem]) -> Vec<ProcTree> {
    let by_pid: HashMap<u32, &ProcMem> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut roots: Vec<&ProcMem> = procs
        .iter()
        .filter(|p| p.ppid == 0 || !by_pid.contains_key(&p.ppid))
        .collect();
    roots.sort_by_key(|p| p.pid);
    roots.iter().map(|r| build_node(r, procs)).collect()
}

/// 递归构建一个进程节点及其子树（子进程按 PID 升序）。
fn build_node(p: &ProcMem, procs: &[ProcMem]) -> ProcTree {
    let mut children: Vec<&ProcMem> = procs.iter().filter(|c| c.ppid == p.pid).collect();
    children.sort_by_key(|c| c.pid);
    ProcTree {
        pid: p.pid,
        name: p.name.clone(),
        children: children.iter().map(|c| build_node(c, procs)).collect(),
    }
}

/// 节点标签：`comm(pid)`。
fn node_label(node: &ProcTree) -> String {
    format!("{}({})", node.name, node.pid)
}

/// 渲染进程树（tree 风格：根直显，子节点 ├──/└──，祖先用 │ 延续）。
pub(crate) fn render_process_tree(roots: &[ProcTree]) -> Vec<String> {
    let mut out = Vec::new();
    for root in roots {
        out.push(node_label(root));
        render_children(root, "", &mut out);
    }
    out
}

/// 递归渲染节点的子进程。
fn render_children(node: &ProcTree, prefix: &str, out: &mut Vec<String>) {
    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let last = i == n - 1;
        let conn = if last { "└── " } else { "├── " };
        out.push(format!("{}{}{}", prefix, conn, node_label(child)));
        let child_prefix = format!("{}{}", prefix, if last { "    " } else { "│   " });
        render_children(child, &child_prefix, out);
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

    #[test]
    fn name_and_help() {
        assert_eq!(PROCESSES.name(), "processes");
        assert!(PROCESSES.help().contains("tree"));
    }

    #[test]
    fn build_tree_roots_and_children() {
        let procs = vec![
            proc(1, 0, "init"),
            proc(2, 0, "kthreadd"),
            proc(3, 2, "kworker"),
            proc(49, 1, "rgetty"),
            proc(50, 49, "sh"),
        ];
        let roots = build_process_tree(&procs);
        // 根：init(1)、kthreadd(2)（ppid 0），按 PID 升序
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].pid, 1);
        assert_eq!(roots[1].pid, 2);
        // init 的子：rgetty(49)；rgetty 的子：sh(50)
        assert_eq!(roots[0].children[0].pid, 49);
        assert_eq!(roots[0].children[0].children[0].pid, 50);
        // kthreadd 的子：kworker(3)
        assert_eq!(roots[1].children[0].pid, 3);
    }

    #[test]
    fn build_tree_orphan_is_root() {
        // ppid 不存在的进程也作为根（如父进程已退出）
        let procs = vec![proc(1, 0, "init"), proc(42, 99, "orphan")];
        let roots = build_process_tree(&procs);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[1].pid, 42);
    }

    #[test]
    fn build_tree_children_sorted_by_pid() {
        let procs = vec![proc(1, 0, "init"), proc(5, 1, "b"), proc(3, 1, "a")];
        let roots = build_process_tree(&procs);
        assert_eq!(roots[0].children[0].pid, 3);
        assert_eq!(roots[0].children[1].pid, 5);
    }

    #[test]
    fn render_tree_shape() {
        let procs = vec![
            proc(1, 0, "init"),
            proc(2, 0, "kthreadd"),
            proc(3, 2, "kworker"),
            proc(49, 1, "rgetty"),
            proc(50, 49, "sh"),
            proc(51, 1, "shell2"),
        ];
        let roots = build_process_tree(&procs);
        let lines = render_process_tree(&roots);
        assert_eq!(lines[0], "init(1)");
        assert_eq!(lines[1], "├── rgetty(49)");
        assert_eq!(lines[2], "│   └── sh(50)");
        assert_eq!(lines[3], "└── shell2(51)");
        assert_eq!(lines[4], "kthreadd(2)");
        assert_eq!(lines[5], "└── kworker(3)");
    }

    #[test]
    fn render_tree_single_root() {
        let procs = vec![proc(1, 0, "init")];
        let roots = build_process_tree(&procs);
        let lines = render_process_tree(&roots);
        assert_eq!(lines, vec!["init(1)"]);
    }
}
