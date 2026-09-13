//! 作业控制：后台/挂起任务表（`jobs`/`fg`/`bg`/`wait`/`disown`）。
//!
//! 表为进程内全局状态（shell 单线程执行，Mutex 仅满足 Sync）。
//! SIGCHLD 处理器只置位标志（async-signal-safe），由 [`reap_children`] 在
//! 安全点回收并记录退出状态；`wait`/`jobs` 优先使用记录的状态。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// 已回收子进程的退出状态（pid -> 状态码，128+signal 编码）。
fn statuses() -> &'static Mutex<HashMap<i32, i32>> {
    static STATUSES: OnceLock<Mutex<HashMap<i32, i32>>> = OnceLock::new();
    STATUSES.get_or_init(|| Mutex::new(HashMap::new()))
}

static SIGCHLD_PENDING: AtomicBool = AtomicBool::new(false);

/// SIGCHLD 处理器：仅置位（async-signal-safe）。
pub(crate) extern "C" fn sigchld_handler(_sig: i32) {
    SIGCHLD_PENDING.store(true, Ordering::SeqCst);
}

/// 是否有待处理的 SIGCHLD。
pub(crate) fn sigchld_pending() -> bool {
    SIGCHLD_PENDING.load(Ordering::SeqCst)
}

/// 把 waitpid 状态转为退出码（信号终止 = 128+signal）。
fn status_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        0
    }
}

/// 回收所有已退出/停止/继续的子进程并记录状态。
/// 返回本次回收的数量。停止/继续事件用于更新作业状态。
pub(crate) fn reap_children() -> usize {
    let mut count = 0;
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe {
            libc::waitpid(
                -1,
                &mut status,
                libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
            )
        };
        if pid <= 0 {
            break;
        }
        count += 1;
        let pid = pid as i32;
        if libc::WIFSTOPPED(status) {
            update_state_by_pid(pid, JobState::Stopped);
        } else if libc::WIFCONTINUED(status) {
            update_state_by_pid(pid, JobState::Running);
        } else {
            if let Ok(mut s) = statuses().lock() {
                s.insert(pid, status_code(status));
            }
        }
    }
    count
}

/// 按 pid（作业组长 = pgid）更新状态。
fn update_state_by_pid(pid: i32, state: JobState) {
    if let Ok(mut t) = table().lock()
        && let Some(job) = t.iter_mut().find(|j| j.pgid == pid)
    {
        job.state = state;
    }
}

/// 取走指定 pid 的退出状态。
pub(crate) fn take_status(pid: i32) -> Option<i32> {
    statuses().lock().ok()?.remove(&pid)
}

/// 查询指定 pid 是否有已记录状态。
#[cfg(test)]
pub(crate) fn has_status(pid: i32) -> bool {
    statuses()
        .lock()
        .map(|s| s.contains_key(&pid))
        .unwrap_or(false)
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

/// 解析作业规格：`%n`、`%+`/`%%`（最近）、`%-`（次近）、`%?str`、`%str`。
/// 返回表中下标。
fn resolve_spec(t: &[Job], spec: Option<&str>) -> Option<usize> {
    let Some(s) = spec else {
        return if t.is_empty() {
            None
        } else {
            Some(t.len() - 1)
        };
    };
    let s = s.strip_prefix('%').unwrap_or(s);
    match s {
        "" | "+" | "%" => {
            if t.is_empty() {
                None
            } else {
                Some(t.len() - 1)
            }
        }
        "-" => {
            if t.len() >= 2 {
                Some(t.len() - 2)
            } else {
                None
            }
        }
        _ => {
            if let Ok(id) = s.parse::<u32>() {
                return t.iter().position(|j| j.id == id);
            }
            if let Some(pat) = s.strip_prefix('?') {
                t.iter().position(|j| j.command.contains(pat))
            } else {
                t.iter().position(|j| j.command.contains(s))
            }
        }
    }
}

/// 取出一个作业（fg/wait 用）。
pub(crate) fn take(spec: Option<&str>) -> Option<Job> {
    let Ok(mut t) = table().lock() else {
        return None;
    };
    t.retain(|j| pgid_alive(j.pgid));
    let idx = resolve_spec(&t, spec)?;
    Some(t.remove(idx))
}

/// 查找作业（不移除）。
pub(crate) fn find(spec: Option<&str>) -> Option<Job> {
    let Ok(mut t) = table().lock() else {
        return None;
    };
    t.retain(|j| pgid_alive(j.pgid));
    let idx = resolve_spec(&t, spec)?;
    Some(t[idx].clone())
}

/// 标记作业为运行中（bg 用，不移除）。
pub(crate) fn mark_running(spec: Option<&str>) -> Option<Job> {
    let Ok(mut t) = table().lock() else {
        return None;
    };
    t.retain(|j| pgid_alive(j.pgid));
    let idx = resolve_spec(&t, spec)?;
    t[idx].state = JobState::Running;
    Some(t[idx].clone())
}

/// 从作业表移除（disown）。
pub(crate) fn disown(spec: Option<&str>) -> bool {
    let Ok(mut t) = table().lock() else {
        return false;
    };
    let Some(idx) = resolve_spec(&t, spec) else {
        return false;
    };
    t.remove(idx);
    true
}

/// 向进程组发送 SIGCONT 继续执行。
pub(crate) fn resume(pgid: i32) {
    unsafe {
        libc::kill(-pgid, libc::SIGCONT);
    }
}

/// 等待指定 pid 退出；返回退出码（有记录状态时优先）。
pub(crate) fn wait_pid(pid: i32) -> i32 {
    loop {
        reap_children();
        if let Some(code) = take_status(pid) {
            return code;
        }
        if !pgid_alive(pid) {
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// 阻塞等待进程组退出；返回退出码（有记录状态时优先）。
pub(crate) fn wait_pgid(pgid: i32) -> i32 {
    loop {
        reap_children();
        if let Some(code) = take_status(pgid) {
            return code;
        }
        if !pgid_alive(pgid) {
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// 等待全部后台作业结束；返回最后一个的状态。
pub(crate) fn wait_all() -> i32 {
    let mut last = 0;
    loop {
        reap_children();
        let jobs = list();
        if jobs.is_empty() {
            break;
        }
        for j in &jobs {
            if let Some(code) = take_status(j.pgid) {
                last = code;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    last
}

/// `jobs` 输出行（show_pid 时附 PID）。
pub(crate) fn format_lines(show_pid: bool) -> Vec<String> {
    list()
        .iter()
        .map(|j| {
            let state = match j.state {
                JobState::Running => "Running",
                JobState::Stopped => "Stopped",
            };
            if show_pid {
                format!("[{}] {} {}  {}", j.id, j.pgid, state, j.command)
            } else {
                format!("[{}] {}  {}", j.id, state, j.command)
            }
        })
        .collect()
}

/// 清空作业表与状态（测试用）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    if let Ok(mut t) = table().lock() {
        t.clear();
    }
    if let Ok(mut s) = statuses().lock() {
        s.clear();
    }
    SIGCHLD_PENDING.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(cmd: &str) -> u32 {
        add_job(std::process::id() as i32, cmd, JobState::Running)
    }

    #[test]
    fn add_and_list_job() {
        reset_for_test();
        let id = add("sleep 100");
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
        reset_for_test();
        let id1 = add("job1");
        let id2 = add("job2");
        assert!(id2 > id1);
        // 未取出前 %- 指向次近作业
        assert_eq!(find(Some("%-")).unwrap().id, id1);
        assert_eq!(take(None).unwrap().id, id2);
        assert_eq!(take(None).unwrap().id, id1);
    }

    #[test]
    fn job_specs() {
        reset_for_test();
        let id1 = add("alpha process");
        let id2 = add("beta process");
        assert_eq!(find(Some(&format!("%{}", id1))).unwrap().id, id1);
        assert_eq!(find(Some("%+")).unwrap().id, id2);
        assert_eq!(find(Some("%%")).unwrap().id, id2);
        assert_eq!(find(Some("%-")).unwrap().id, id1);
        assert_eq!(find(Some("%?beta")).unwrap().id, id2);
        assert_eq!(find(Some("%alpha")).unwrap().id, id1);
        assert!(find(Some("%nope")).is_none());
    }

    #[test]
    fn disown_removes_job() {
        reset_for_test();
        let id = add("to disown");
        assert!(disown(Some(&format!("%{}", id))));
        assert!(find(Some(&format!("%{}", id))).is_none());
    }

    #[test]
    fn dead_pgid_pruned() {
        reset_for_test();
        let _ = add_job(-1, "ghost", JobState::Running);
        assert!(list().iter().all(|j| j.command != "ghost"));
    }

    #[test]
    fn status_record_and_take() {
        reset_for_test();
        statuses().lock().unwrap().insert(4242, 7);
        assert!(has_status(4242));
        assert_eq!(take_status(4242), Some(7));
        assert!(!has_status(4242));
    }

    #[test]
    fn format_lines_contains_state() {
        reset_for_test();
        let id = add_job(std::process::id() as i32, "fmt_job", JobState::Stopped);
        let lines = format_lines(false);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Stopped") && l.contains("fmt_job"))
        );
        let lines = format_lines(true);
        assert!(lines.iter().any(|l| l.contains(&format!("[{}]", id))));
        let _ = take(Some(&id.to_string()));
    }

    #[test]
    fn sigchld_flag_roundtrip() {
        reset_for_test();
        assert!(!sigchld_pending());
        sigchld_handler(libc::SIGCHLD);
        assert!(sigchld_pending());
        reset_for_test();
        assert!(!sigchld_pending());
    }
}
