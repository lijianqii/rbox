//! 位置参数：`set -- a b c` / `shift` / `$1..$9` / `$#` / `$@` / `$*`。

use std::sync::{Mutex, OnceLock};

/// `$0`（脚本名/命令名）。
fn arg0() -> &'static Mutex<String> {
    static ARG0: OnceLock<Mutex<String>> = OnceLock::new();
    ARG0.get_or_init(|| Mutex::new("sh".to_string()))
}

/// 设置 `$0`。
pub(crate) fn set0(name: &str) {
    if let Ok(mut a) = arg0().lock() {
        *a = name.to_string();
    }
}

/// 取 `$0`（默认 "sh"）。
pub(crate) fn get0() -> String {
    arg0()
        .lock()
        .map(|a| a.clone())
        .unwrap_or_else(|_| "sh".to_string())
}

fn params() -> &'static Mutex<Vec<String>> {
    static PARAMS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    PARAMS.get_or_init(|| Mutex::new(Vec::new()))
}

/// 设置位置参数（`set -- args...`）。
pub(crate) fn set(args: Vec<String>) {
    if let Ok(mut p) = params().lock() {
        *p = args;
    }
}

/// 取第 index 个位置参数（0 = `$1`）。
pub(crate) fn get(index: usize) -> Option<String> {
    params().lock().ok()?.get(index).cloned()
}

/// 最近一个后台作业的 pid（`$!`）。
static LAST_BG: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// 记录最近的后台进程组（`$!`）。
pub(crate) fn set_last_bg(pid: i32) {
    LAST_BG.store(pid, std::sync::atomic::Ordering::SeqCst);
}

/// `$!`：最近后台 pid（无则 0）。
pub(crate) fn last_bg() -> i32 {
    LAST_BG.load(std::sync::atomic::Ordering::SeqCst)
}

/// `$#`。
pub(crate) fn count() -> usize {
    params().lock().map(|p| p.len()).unwrap_or(0)
}

/// `$@` / `$*`（以空格连接）。
pub(crate) fn all() -> Vec<String> {
    params().lock().map(|p| p.clone()).unwrap_or_default()
}

/// `shift [n]`：移除前 n 个位置参数；n 超出返回 false。
pub(crate) fn shift(n: usize) -> bool {
    let Ok(mut p) = params().lock() else {
        return false;
    };
    if n > p.len() {
        return false;
    }
    p.drain(..n);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_count_all() {
        set(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(count(), 3);
        assert_eq!(get(0).as_deref(), Some("a"));
        assert_eq!(get(2).as_deref(), Some("c"));
        assert_eq!(get(3), None);
        assert_eq!(all(), vec!["a", "b", "c"]);
        set(Vec::new());
        assert_eq!(count(), 0);
    }

    #[test]
    fn shift_removes_prefix() {
        set(vec!["a".into(), "b".into(), "c".into()]);
        assert!(shift(1));
        assert_eq!(all(), vec!["b", "c"]);
        assert!(shift(2));
        assert_eq!(count(), 0);
        assert!(!shift(1));
    }

    #[test]
    fn shift_too_many_fails() {
        set(vec!["a".into()]);
        assert!(!shift(2));
        assert_eq!(count(), 1);
        set(Vec::new());
    }
}
