//! Shell 函数：定义、调用、局部变量（`local`）。

use super::{options, params, script};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn functions() -> &'static Mutex<HashMap<String, String>> {
    static FUNCTIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    FUNCTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 定义（或覆盖）函数。
pub(crate) fn define(name: &str, body: &str) {
    if let Ok(mut f) = functions().lock() {
        f.insert(name.to_string(), body.to_string());
    }
}

/// 取函数体。
pub(crate) fn get(name: &str) -> Option<String> {
    functions().lock().ok()?.get(name).cloned()
}

/// 是否为已定义函数。
pub(crate) fn is_function(name: &str) -> bool {
    functions()
        .lock()
        .map(|f| f.contains_key(name))
        .unwrap_or(false)
}

/// 删除函数（`unset -f`）。
pub(crate) fn unset(name: &str) -> bool {
    functions()
        .lock()
        .map(|mut f| f.remove(name).is_some())
        .unwrap_or(false)
}

/// 局部变量记录：(名字, 旧值)。
type LocalVar = (String, Option<String>);

/// 函数局部变量栈。
fn locals() -> &'static Mutex<Vec<LocalVar>> {
    static LOCALS: OnceLock<Mutex<Vec<LocalVar>>> = OnceLock::new();
    LOCALS.get_or_init(|| Mutex::new(Vec::new()))
}

/// 当前局部变量栈深度（函数入口记录，退出时恢复）。
pub(crate) fn local_mark() -> usize {
    locals().lock().map(|l| l.len()).unwrap_or(0)
}

/// 声明局部变量：记录旧值，当前值由调用方设置。
pub(crate) fn push_local(name: &str) {
    if let Ok(mut l) = locals().lock() {
        let old = std::env::var(name).ok();
        l.push((name.to_string(), old));
    }
}

/// 恢复到指定深度（函数返回时）。
pub(crate) fn pop_locals_to(mark: usize) {
    let Ok(mut l) = locals().lock() else {
        return;
    };
    while l.len() > mark {
        let Some((name, old)) = l.pop() else {
            break;
        };
        unsafe {
            match old {
                Some(v) => std::env::set_var(&name, v),
                None => std::env::remove_var(&name),
            }
        }
    }
}

/// 调用函数：保存位置参数与局部变量，执行函数体，恢复现场。
/// 返回退出码；函数未定义返回 None。
pub(crate) fn call(
    name: &str,
    args: &[String],
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> Option<i32> {
    let body = get(name)?;
    let saved_params = params::all();
    let mark = local_mark();
    params::set(args.to_vec());
    let rc = script::run_source(&body, history, exit_fn, false);
    pop_locals_to(mark);
    params::set(saved_params);
    // return 已在 run_source 内消费；防御性清理
    let _ = options::take_return();
    Some(rc)
}

/// 清空函数与局部变量（测试用）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    if let Ok(mut f) = functions().lock() {
        f.clear();
    }
    if let Ok(mut l) = locals().lock() {
        l.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局局部变量栈在并行测试下互相干扰，串行化。
    fn local_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn define_get_list_unset() {
        reset_for_test();
        define("greet", "echo hi");
        assert!(is_function("greet"));
        assert_eq!(get("greet").as_deref(), Some("echo hi"));
        assert!(unset("greet"));
        assert!(!is_function("greet"));
    }

    #[test]
    fn locals_restore() {
        let _g = local_guard();
        reset_for_test();
        let mark = local_mark();
        unsafe { std::env::set_var("RBOX_LOCAL_TEST", "outer") };
        push_local("RBOX_LOCAL_TEST");
        unsafe { std::env::set_var("RBOX_LOCAL_TEST", "inner") };
        assert_eq!(std::env::var("RBOX_LOCAL_TEST").unwrap(), "inner");
        pop_locals_to(mark);
        assert_eq!(std::env::var("RBOX_LOCAL_TEST").unwrap(), "outer");
        unsafe { std::env::remove_var("RBOX_LOCAL_TEST") };
    }

    #[test]
    fn locals_remove_new_vars() {
        let _g = local_guard();
        reset_for_test();
        let mark = local_mark();
        push_local("RBOX_LOCAL_NEW");
        unsafe { std::env::set_var("RBOX_LOCAL_NEW", "x") };
        pop_locals_to(mark);
        assert!(std::env::var("RBOX_LOCAL_NEW").is_err());
    }
}
