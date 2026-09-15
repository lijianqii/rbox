//! Shell 选项与运行时状态：`set -e/-x/-u/-o pipefail`、退出请求、nounset 违规。
//!
//! 用全局原子量保存（shell 单线程执行；脚本/交互共用）。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static ERREXIT: AtomicBool = AtomicBool::new(false);
static XTRACE: AtomicBool = AtomicBool::new(false);
static NOUNSET: AtomicBool = AtomicBool::new(false);
static PIPEFAIL: AtomicBool = AtomicBool::new(false);
static NOCLOBBER: AtomicBool = AtomicBool::new(false);
static NOGLOB: AtomicBool = AtomicBool::new(false);
static VERBOSE: AtomicBool = AtomicBool::new(false);
static NOEXEC: AtomicBool = AtomicBool::new(false);
static ALLEXPORT: AtomicBool = AtomicBool::new(false);
static MONITOR: AtomicBool = AtomicBool::new(false);
static NOTIFY: AtomicBool = AtomicBool::new(false);
static IGNOREEOF: AtomicBool = AtomicBool::new(false);
static BRACEEXPAND: AtomicBool = AtomicBool::new(true);
static PHYSICAL: AtomicBool = AtomicBool::new(false);
static HISTORY: AtomicBool = AtomicBool::new(true);
/// 交互式 shell 标志（别名展开等仅交互模式生效）。
static INTERACTIVE: AtomicBool = AtomicBool::new(false);
/// shell 进程号（`$$`）：子 shell 沿用父 shell 的 pid（POSIX/ash 行为）。
static SHELL_PID: AtomicI32 = AtomicI32::new(0);
/// nounset 违规标记：expand_vars 发现未定义变量时置位，脚本驱动据此退出。
static NOUNSET_VIOLATION: AtomicBool = AtomicBool::new(false);
/// 退出请求（`exit` 之外的内部退出：set -e 触发等）；-1 表示无。
static EXIT_REQUESTED: AtomicI32 = AtomicI32::new(-1);
/// `return` 请求（source/函数帧消费）；-1 表示无。
static RETURN_REQUESTED: AtomicI32 = AtomicI32::new(-1);

/// `$$` 是否尚未初始化（用于区分顶层 shell 与 fork 子 shell）。
/// 单字母选项设置（`set -e` 等；供 subshell 状态恢复复用）。
pub(crate) fn set_by_flag(ch: char, on: bool) -> bool {
    match ch {
        'a' => set_allexport(on),
        'b' => set_notify(on),
        'C' => set_noclobber(on),
        'e' => set_errexit(on),
        'f' => set_noglob(on),
        'm' => set_monitor(on),
        'n' => set_noexec(on),
        'u' => set_nounset(on),
        'v' => set_verbose(on),
        'x' => set_xtrace(on),
        _ => return false,
    }
    true
}

pub(crate) fn shell_pid_unset() -> bool {
    SHELL_PID.load(Ordering::SeqCst) == 0
}
pub(crate) fn shell_pid() -> i32 {
    let p = SHELL_PID.load(Ordering::SeqCst);
    if p > 0 { p } else { std::process::id() as i32 }
}
pub(crate) fn set_shell_pid(pid: i32) {
    SHELL_PID.store(pid, Ordering::SeqCst);
}

pub(crate) fn interactive() -> bool {
    INTERACTIVE.load(Ordering::SeqCst)
}
pub(crate) fn set_interactive(v: bool) {
    INTERACTIVE.store(v, Ordering::SeqCst);
}
pub(crate) fn braceexpand() -> bool {
    BRACEEXPAND.load(Ordering::SeqCst)
}
pub(crate) fn physical() -> bool {
    PHYSICAL.load(Ordering::SeqCst)
}
pub(crate) fn history_enabled() -> bool {
    HISTORY.load(Ordering::SeqCst)
}
pub(crate) fn set_history(v: bool) {
    HISTORY.store(v, Ordering::SeqCst);
}

pub(crate) fn errexit() -> bool {
    ERREXIT.load(Ordering::SeqCst)
}
pub(crate) fn set_errexit(v: bool) {
    ERREXIT.store(v, Ordering::SeqCst);
}
pub(crate) fn xtrace() -> bool {
    XTRACE.load(Ordering::SeqCst)
}
pub(crate) fn set_xtrace(v: bool) {
    XTRACE.store(v, Ordering::SeqCst);
}
pub(crate) fn nounset() -> bool {
    NOUNSET.load(Ordering::SeqCst)
}
pub(crate) fn set_nounset(v: bool) {
    NOUNSET.store(v, Ordering::SeqCst);
}
pub(crate) fn pipefail() -> bool {
    PIPEFAIL.load(Ordering::SeqCst)
}
pub(crate) fn set_pipefail(v: bool) {
    PIPEFAIL.store(v, Ordering::SeqCst);
}
pub(crate) fn noclobber() -> bool {
    NOCLOBBER.load(Ordering::SeqCst)
}
pub(crate) fn set_noclobber(v: bool) {
    NOCLOBBER.store(v, Ordering::SeqCst);
}
pub(crate) fn noglob() -> bool {
    NOGLOB.load(Ordering::SeqCst)
}
pub(crate) fn set_noglob(v: bool) {
    NOGLOB.store(v, Ordering::SeqCst);
}
pub(crate) fn verbose() -> bool {
    VERBOSE.load(Ordering::SeqCst)
}
pub(crate) fn set_verbose(v: bool) {
    VERBOSE.store(v, Ordering::SeqCst);
}
pub(crate) fn noexec() -> bool {
    NOEXEC.load(Ordering::SeqCst)
}
pub(crate) fn set_noexec(v: bool) {
    NOEXEC.store(v, Ordering::SeqCst);
}
pub(crate) fn allexport() -> bool {
    ALLEXPORT.load(Ordering::SeqCst)
}
pub(crate) fn set_allexport(v: bool) {
    ALLEXPORT.store(v, Ordering::SeqCst);
}
pub(crate) fn monitor() -> bool {
    MONITOR.load(Ordering::SeqCst)
}
pub(crate) fn set_monitor(v: bool) {
    MONITOR.store(v, Ordering::SeqCst);
}
pub(crate) fn notify() -> bool {
    NOTIFY.load(Ordering::SeqCst)
}
pub(crate) fn set_notify(v: bool) {
    NOTIFY.store(v, Ordering::SeqCst);
}
pub(crate) fn ignoreeof() -> bool {
    IGNOREEOF.load(Ordering::SeqCst)
}
pub(crate) fn set_ignoreeof(v: bool) {
    IGNOREEOF.store(v, Ordering::SeqCst);
}

/// `$-`：当前选项字母（ash 风格：a b C e f m n u v x）。
pub(crate) fn option_string() -> String {
    let mut out = String::new();
    if allexport() {
        out.push('a');
    }
    if notify() {
        out.push('b');
    }
    if noclobber() {
        out.push('C');
    }
    if errexit() {
        out.push('e');
    }
    if noglob() {
        out.push('f');
    }
    if monitor() {
        out.push('m');
    }
    if noexec() {
        out.push('n');
    }
    if nounset() {
        out.push('u');
    }
    if verbose() {
        out.push('v');
    }
    if xtrace() {
        out.push('x');
    }
    out
}

/// `set -o` 可设置选项名 -> 当前值。
pub(crate) fn named_options() -> Vec<(&'static str, bool)> {
    vec![
        ("allexport", allexport()),
        ("braceexpand", braceexpand()),
        ("emacs", false),
        ("errexit", errexit()),
        ("errtrace", false),
        ("functrace", false),
        ("history", history_enabled()),
        ("interactive", unsafe { libc::isatty(0) } == 1),
        ("ignoreeof", ignoreeof()),
        ("monitor", monitor()),
        ("noclobber", noclobber()),
        ("noexec", noexec()),
        ("noglob", noglob()),
        ("nolog", false),
        ("keyword", false),
        ("notify", notify()),
        ("nounset", nounset()),
        ("onecmd", false),
        ("physical", physical()),
        ("pipefail", pipefail()),
        ("posix", false),
        ("privileged", false),
        ("verbose", verbose()),
        ("vi", false),
        ("xtrace", xtrace()),
    ]
}

/// 设置命名选项；返回是否成功。
pub(crate) fn set_named(name: &str, on: bool) -> bool {
    match name {
        "allexport" => set_allexport(on),
        "emacs" => {}
        "errexit" => set_errexit(on),
        "errtrace" => {}
        "functrace" => {}
        "history" => set_history(on),
        "interactive" => {}
        "ignoreeof" => set_ignoreeof(on),
        "monitor" => set_monitor(on),
        "noclobber" => set_noclobber(on),
        "noexec" => set_noexec(on),
        "noglob" => set_noglob(on),
        "keyword" => {}
        "nolog" => {}
        "notify" => set_notify(on),
        "nounset" => set_nounset(on),
        "onecmd" => {}
        "pipefail" => set_pipefail(on),
        "posix" => {}
        "privileged" => {}
        "verbose" => set_verbose(on),
        "vi" => {}
        "xtrace" => set_xtrace(on),
        _ => return false,
    }
    true
}

/// 记录 nounset 违规（未定义变量展开）。
pub(crate) fn mark_nounset_violation() {
    NOUNSET_VIOLATION.store(true, Ordering::SeqCst);
}
/// 是否有 nounset 违规（不消费）。
pub(crate) fn nounset_violation() -> bool {
    NOUNSET_VIOLATION.load(Ordering::SeqCst)
}

/// 取走并清除 nounset 违规标记。
pub(crate) fn take_nounset_violation() -> bool {
    NOUNSET_VIOLATION.swap(false, Ordering::SeqCst)
}

/// 请求退出（脚本模式下 set -e 等内部触发）。
pub(crate) fn request_exit(code: i32) {
    EXIT_REQUESTED.store(code, Ordering::SeqCst);
}
/// 取走退出请求。
pub(crate) fn take_exit_request() -> Option<i32> {
    let v = EXIT_REQUESTED.swap(-1, Ordering::SeqCst);
    if v < 0 { None } else { Some(v) }
}
/// 请求 `return n`（source/函数帧消费）。
pub(crate) fn request_return(code: i32) {
    RETURN_REQUESTED.store(code, Ordering::SeqCst);
}
/// 取走 return 请求。
pub(crate) fn take_return() -> Option<i32> {
    let v = RETURN_REQUESTED.swap(-1, Ordering::SeqCst);
    if v < 0 { None } else { Some(v) }
}
/// 是否有待处理的 return 请求（不消费）。
pub(crate) fn return_requested() -> bool {
    RETURN_REQUESTED.load(Ordering::SeqCst) >= 0
}

/// 重置全部选项与状态（测试用）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    ERREXIT.store(false, Ordering::SeqCst);
    XTRACE.store(false, Ordering::SeqCst);
    NOUNSET.store(false, Ordering::SeqCst);
    PIPEFAIL.store(false, Ordering::SeqCst);
    NOCLOBBER.store(false, Ordering::SeqCst);
    NOUNSET_VIOLATION.store(false, Ordering::SeqCst);
    EXIT_REQUESTED.store(-1, Ordering::SeqCst);
    RETURN_REQUESTED.store(-1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_toggles() {
        reset_for_test();
        assert!(!errexit());
        set_errexit(true);
        assert!(errexit());
        assert!(!xtrace());
        set_xtrace(true);
        assert!(xtrace());
        assert!(!nounset());
        set_nounset(true);
        assert!(nounset());
        assert!(!pipefail());
        set_pipefail(true);
        assert!(pipefail());
        reset_for_test();
        assert!(!errexit() && !xtrace() && !nounset() && !pipefail());
    }

    #[test]
    fn nounset_violation_flag() {
        reset_for_test();
        assert!(!take_nounset_violation());
        mark_nounset_violation();
        assert!(take_nounset_violation());
        assert!(!take_nounset_violation());
    }

    #[test]
    fn exit_request() {
        reset_for_test();
        assert_eq!(take_exit_request(), None);
        request_exit(3);
        assert_eq!(take_exit_request(), Some(3));
        assert_eq!(take_exit_request(), None);
    }

    #[test]
    fn return_request() {
        reset_for_test();
        assert!(!return_requested());
        request_return(5);
        assert!(return_requested());
        assert_eq!(take_return(), Some(5));
        assert_eq!(take_return(), None);
    }

    #[test]
    fn option_string_and_named_options() {
        let _g = crate::applets::core::shell::compound::tests::test_guard();
        reset_for_test();
        set_noclobber(true);
        set_noglob(true);
        let flags = option_string();
        assert!(flags.contains('C'), "$- 应含 noclobber: {}", flags);
        assert!(flags.contains('f'), "$- 应含 noglob: {}", flags);
        assert!(set_named("pipefail", true));
        assert!(pipefail());
        assert!(named_options().iter().any(|(n, v)| *n == "pipefail" && *v));
        assert!(named_options().iter().any(|(n, _)| *n == "noclobber"));
        for name in [
            "braceexpand",
            "emacs",
            "errtrace",
            "functrace",
            "history",
            "interactive",
            "keyword",
            "onecmd",
            "physical",
            "posix",
            "privileged",
        ] {
            assert!(
                named_options().iter().any(|(n, _)| *n == name),
                "set -o 缺少 {}",
                name
            );
        }
        // ash 实测：braceexpand/physical 可列出但不可设置
        assert!(!set_named("braceexpand", false));
        assert!(!set_named("physical", true));
        assert!(set_named("ignoreeof", true));
        assert!(ignoreeof());
        assert!(set_named("ignoreeof", false));
        assert!(!set_named("bogus_option", true));
        assert!(set_named("pipefail", false));
        assert!(!pipefail());
        reset_for_test();
    }
}
