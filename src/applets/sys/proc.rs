//! 进程信息共享工具：从 /proc 收集进程并解析（meminfo / processes 共用）。

/// 一个进程的内存信息（VSZ/RSS 单位 kB）。
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
    /// 可执行文件路径（/proc/<pid>/exe；内核线程/不可读时为空）
    pub(crate) exe: String,
}

/// 遍历 /proc 收集所有进程的信息（statm 的 size/resident + stat 的 ppid/state）。
pub(crate) fn collect_processes() -> Vec<ProcMem> {
    let page_kb = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64) / 1024;
    let mut out = Vec::new();
    let proc_root = &crate::config::load().paths.proc;
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let (size_pages, rss_pages) = parse_statm(&read_proc_file(pid, "statm"));
        let ppid = parse_stat_ppid(&read_proc_file(pid, "stat"));
        let state = parse_stat_state(&read_proc_file(pid, "stat"));
        let comm = read_proc_file(pid, "comm").trim().to_string();
        let exe = read_proc_exe(pid);
        out.push(ProcMem {
            pid,
            ppid,
            vsz_kb: size_pages * page_kb,
            rss_kb: rss_pages * page_kb,
            state,
            name: comm,
            exe,
        });
    }
    out
}

/// 按 RSS 降序、同值按 PID 升序排序。
pub(crate) fn sort_processes(procs: &mut [ProcMem]) {
    procs.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb).then(a.pid.cmp(&b.pid)));
}

/// 读取系统总内存（kB，/proc/meminfo 的 MemTotal）；失败返回 0。
pub(crate) fn mem_total_kb() -> u64 {
    let path = &crate::config::load().paths.meminfo;
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| {
            c.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim() != "MemTotal" {
                    return None;
                }
                v.split_whitespace().next()?.parse().ok()
            })
        })
        .unwrap_or(0)
}

/// 读取 /proc/<pid>/exe 符号链接（路径根可配置）；
/// 失败（内核线程/受限环境）时回退 cmdline 的 argv[0]，再失败返回空串。
fn read_proc_exe(pid: u32) -> String {
    let proc_root = &crate::config::load().paths.proc;
    let exe = std::fs::read_link(format!("{}/{}/exe", proc_root, pid))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !exe.is_empty() {
        return exe;
    }
    std::fs::read(format!("{}/{}/cmdline", proc_root, pid))
        .ok()
        .map(|b| cmdline_argv0(&b))
        .unwrap_or_default()
}

/// 取 cmdline 字节中的第一个参数（argv[0]，NUL 分隔）。
pub(crate) fn cmdline_argv0(bytes: &[u8]) -> String {
    bytes
        .split(|&b| b == 0)
        .next()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .unwrap_or_default()
}

/// 读取 /proc/<pid>/<file> 内容（路径根可配置）；失败返回空串。
fn read_proc_file(pid: u32, file: &str) -> String {
    let proc_root = &crate::config::load().paths.proc;
    std::fs::read_to_string(format!("{}/{}/{}", proc_root, pid, file)).unwrap_or_default()
}

/// 解析 statm 文本，返回 (size 页数, resident 页数)（第 1、2 个字段）。
pub(crate) fn parse_statm(content: &str) -> (u64, u64) {
    let mut it = content.split_whitespace();
    let size = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let resident = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (size, resident)
}

/// stat 文本中 ")" 之后的字段（comm 可能含空格/括号）。
fn stat_tail(content: &str) -> Vec<&str> {
    content
        .split(')')
        .nth(1)
        .map(|s| s.split_whitespace().collect())
        .unwrap_or_default()
}

/// 解析 stat 文本，返回 ppid（")" 后第 2 个字段）。
pub(crate) fn parse_stat_ppid(content: &str) -> u32 {
    stat_tail(content)
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// 解析 stat 文本，返回进程状态（")" 后第 1 个字段，如 S/R/Z）。
pub(crate) fn parse_stat_state(content: &str) -> String {
    stat_tail(content)
        .first()
        .copied()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn cmdline_argv0_parses() {
        assert_eq!(cmdline_argv0(b"hello\0world\0"), "hello");
        assert_eq!(cmdline_argv0(b"rbox\0processes"), "rbox");
        assert_eq!(cmdline_argv0(b""), "");
        assert_eq!(cmdline_argv0(b"\0\0"), "");
    }

    #[test]
    fn sort_processes_by_rss_desc() {
        let mut procs = vec![
            ProcMem {
                pid: 1,
                ppid: 0,
                vsz_kb: 0,
                rss_kb: 100,
                state: "S".into(),
                name: "a".into(),
                exe: "/bin/a".into(),
            },
            ProcMem {
                pid: 2,
                ppid: 1,
                vsz_kb: 0,
                rss_kb: 300,
                state: "S".into(),
                name: "b".into(),
                exe: "/bin/b".into(),
            },
            ProcMem {
                pid: 3,
                ppid: 1,
                vsz_kb: 0,
                rss_kb: 300,
                state: "R".into(),
                name: "c".into(),
                exe: "/bin/c".into(),
            },
        ];
        sort_processes(&mut procs);
        assert_eq!(procs[0].pid, 2);
        assert_eq!(procs[1].pid, 3); // 同 RSS 按 PID 升序
        assert_eq!(procs[2].pid, 1);
    }
}
