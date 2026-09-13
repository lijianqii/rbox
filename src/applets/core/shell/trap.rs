//! `trap`：信号/EXIT 陷阱。
//!
//! - EXIT 陷阱在 shell 退出（exit 内置、脚本结束）时执行；
//! - INT/TERM 等信号由信号处理器记录待处理信号（async-signal-safe 原子量），
//!   REPL/脚本驱动在安全点调用 [`run_pending`] 执行对应命令；
//! - `trap -l` 列出信号，`trap - SIG` 清除。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

/// 待处理信号（-1 = 无）。
static PENDING: AtomicI32 = AtomicI32::new(-1);

fn traps() -> &'static Mutex<HashMap<i32, String>> {
    static TRAPS: OnceLock<Mutex<HashMap<i32, String>>> = OnceLock::new();
    TRAPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 信号名 -> 编号（复用 kill 的映射，支持 EXIT=0）。
pub(crate) fn signal_number(name: &str) -> Option<i32> {
    let upper = name.to_ascii_uppercase();
    if upper == "EXIT" || upper == "0" {
        return Some(0);
    }
    crate::applets::sys::kill::signal_number(&upper)
}

/// 设置陷阱：`trap 'cmd' SIG...`。
pub(crate) fn set(sig: i32, cmd: &str) {
    if let Ok(mut t) = traps().lock() {
        t.insert(sig, cmd.to_string());
    }
}

/// 清除陷阱（`trap - SIG`）。
pub(crate) fn clear(sig: i32) {
    if let Ok(mut t) = traps().lock() {
        t.remove(&sig);
    }
}

/// 取指定信号的陷阱命令。
pub(crate) fn get(sig: i32) -> Option<String> {
    traps().lock().ok()?.get(&sig).cloned()
}

/// 列出已设置的陷阱（`trap` 无参数）。
pub(crate) fn list() -> Vec<(i32, String)> {
    let Ok(t) = traps().lock() else {
        return Vec::new();
    };
    let mut v: Vec<(i32, String)> = t.iter().map(|(k, v)| (*k, v.clone())).collect();
    v.sort();
    v
}

/// 信号处理器：记录待处理信号（仅原子操作，async-signal-safe）。
pub(crate) extern "C" fn handler(sig: i32) {
    PENDING.store(sig, Ordering::SeqCst);
}

/// 安装 INT/TERM/HUP 处理器（若对应陷阱已设置，或总是安装以记录信号）。
pub(crate) fn install_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as usize);
        libc::signal(libc::SIGTERM, handler as *const () as usize);
        libc::signal(libc::SIGHUP, handler as *const () as usize);
    }
}

/// 取走待处理信号。
pub(crate) fn take_pending() -> Option<i32> {
    let v = PENDING.swap(-1, Ordering::SeqCst);
    if v < 0 { None } else { Some(v) }
}

/// 信号编号 -> 名称（用于 `trap` 输出）。
pub(crate) fn signal_name(sig: i32) -> String {
    if sig == 0 {
        return "EXIT".to_string();
    }
    crate::applets::sys::kill::signal_name(sig)
        .map(str::to_string)
        .unwrap_or_else(|| sig.to_string())
}

/// 清除全部陷阱（测试用）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    if let Ok(mut t) = traps().lock() {
        t.clear();
    }
    PENDING.store(-1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_clear_list() {
        reset_for_test();
        assert!(get(libc::SIGTERM).is_none());
        set(libc::SIGTERM, "echo bye");
        assert_eq!(get(libc::SIGTERM).as_deref(), Some("echo bye"));
        let listed = list();
        assert!(
            listed
                .iter()
                .any(|(s, c)| *s == libc::SIGTERM && c == "echo bye")
        );
        clear(libc::SIGTERM);
        assert!(get(libc::SIGTERM).is_none());
    }

    #[test]
    fn exit_signal_number() {
        assert_eq!(signal_number("EXIT"), Some(0));
        assert_eq!(signal_number("exit"), Some(0));
        assert_eq!(signal_number("TERM"), Some(libc::SIGTERM));
        assert_eq!(signal_number("NOPE"), None);
    }

    #[test]
    fn pending_signal() {
        reset_for_test();
        assert_eq!(take_pending(), None);
        handler(libc::SIGINT);
        assert_eq!(take_pending(), Some(libc::SIGINT));
        assert_eq!(take_pending(), None);
    }

    #[test]
    fn signal_names() {
        assert_eq!(signal_name(0), "EXIT");
        assert_eq!(signal_name(libc::SIGTERM), "TERM");
    }
}
