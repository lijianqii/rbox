//! 作业控制：后台/挂起任务表（`jobs`/`fg`/`bg`）。
//!
//! 表为进程内全局状态（shell 单线程执行，Mutex 仅满足 Sync）。
//! 进程组存活用 `kill(pgid, 0)` 探测（SIGCHLD 处理器负责收割僵尸）。

use std::sync::{Mutex, OnceLock};

/// 作业状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum JobState {
    Running,
    Stopped,
}

/// 一个作业（对应一个进程组）。
#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub(crate) id: u32,
    pub(crate) pgid: i32,
    pub(crate) command: String,
    pub(crate) state: JobState,
}

fn table() -> &'static Mutex<Vec<Job>> {
    static JOBS: OnceLock<Mutex<Vec<Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Vec::new()))
}

/// 进程组是否仍存在（EPERM 也算存在：无权限但进程在）。
pub(crate) fn pgid_alive(pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pgid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 登记后台/挂起作业，返回作业号。
pub(crate) fn add_job(pgid: i32, command: &str, state: JobState) -> u32 {
    let Ok(mut t) = table().lock() else {
        return 0;
    };
    t.retain(|j| pgid_alive(j.pgid));
    let id = t.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    t.push(Job {
        id,
        pgid,
        command: command.to_string(),
        state,
    });
    id
}

/// 列出当前作业（自动清理已退出）。
pub(crate) fn list() -> Vec<Job> {
    let Ok(mut t) = table().lock() else {
        return Vec::new();
    };
    t.retain(|j| pgid_alive(j.pgid));
    t.clone()
}

/// 取出一个作业（fg 用；默认取最近一个）。spec 支持 `%n` 或 `n`。
pub(crate) fn take(spec: Option<&str>) -> Option<Job> {
    let Ok(mut t) = table().lock() else {
        return None;
    };
    t.retain(|j| pgid_alive(j.pgid));
    if t.is_empty() {
        return None;
    }
    let idx = match spec {
        None => t.len() - 1,
        Some(s) => {
            let s = s.strip_prefix('%').unwrap_or(s);
            let id: u32 = s.parse().ok()?;
            t.iter().position(|j| j.id == id)?
        }
    };
    Some(t.remove(idx))
}

/// 向进程组发送 SIGCONT 继续执行。
pub(crate) fn resume(pgid: i32) {
    unsafe {
        libc::kill(-pgid, libc::SIGCONT);
    }
}

/// 查找作业并标记为运行中（bg 用，不移除）。
pub(crate) fn mark_running(spec: Option<&str>) -> Option<Job> {
    let Ok(mut t) = table().lock() else {
        return None;
    };
    t.retain(|j| pgid_alive(j.pgid));
    if t.is_empty() {
        return None;
    }
    let idx = match spec {
        None => t.len() - 1,
        Some(s) => {
            let s = s.strip_prefix('%').unwrap_or(s);
            let id: u32 = s.parse().ok()?;
            t.iter().position(|j| j.id == id)?
        }
    };
    t[idx].state = JobState::Running;
    Some(t[idx].clone())
}

/// 阻塞等待进程组退出（轮询；SIGCHLD 处理器负责收割子进程）。
pub(crate) fn wait_pgid(pgid: i32) {
    while pgid_alive(pgid) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `jobs` 输出行。
pub(crate) fn format_lines() -> Vec<String> {
    list()
        .iter()
        .map(|j| {
            let state = match j.state {
                JobState::Running => "Running",
                JobState::Stopped => "Stopped",
            };
            format!("[{}] {}  {}", j.id, state, j.command)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_list_job() {
        let id = add_job(std::process::id() as i32, "sleep 100", JobState::Running);
        assert!(id > 0);
        let listed = list();
        assert!(
            listed
                .iter()
                .any(|j| j.id == id && j.command == "sleep 100")
        );
        let taken = take(Some(&id.to_string())).unwrap();
        assert_eq!(taken.id, id);
    }

    #[test]
    fn take_default_takes_latest() {
        let id1 = add_job(std::process::id() as i32, "job1", JobState::Running);
        let id2 = add_job(std::process::id() as i32, "job2", JobState::Running);
        assert!(id2 > id1);
        let taken = take(None).unwrap();
        assert_eq!(taken.id, id2);
        let _ = take(None); // 清理 id1
    }

    #[test]
    fn take_unknown_returns_none() {
        assert!(take(Some("999999")).is_none());
        assert!(take(Some("not-a-number")).is_none());
    }

    #[test]
    fn dead_pgid_pruned() {
        // 无效 pgid 登记后，list 会自动清理
        let _ = add_job(-1, "ghost", JobState::Running);
        assert!(list().iter().all(|j| j.command != "ghost"));
    }

    #[test]
    fn format_lines_contains_state() {
        let id = add_job(std::process::id() as i32, "fmt_job", JobState::Stopped);
        let lines = format_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Stopped") && l.contains("fmt_job"))
        );
        let _ = take(Some(&id.to_string()));
    }
}
