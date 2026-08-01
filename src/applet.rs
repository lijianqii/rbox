//! Applet trait 与全局注册表。
//!
//! 每个 applet 实现 [`Applet`] trait 并提供静态实例，登记到 [`APPLETS`]。
//! 分发逻辑在 [`crate::main`] 中根据 argv[0] basename 或子命令查表。

use std::process::ExitCode;

/// 一个 applet 命令的行为。
pub trait Applet: Sync {
    /// 命令名，如 `"echo"`、`"cat"`。用于查表分发。
    fn name(&self) -> &'static str;

    /// 简短帮助。
    fn help(&self) -> &'static str {
        ""
    }

    /// 执行 applet。`args` 为去掉程序名/子命令后的参数列表。
    fn run(&self, args: &[String]) -> ExitCode;
}

/// 所有已注册的 applet。分发时线性查找。
pub static APPLETS: &[&dyn Applet] = &[
    crate::applets::true_::TRUE,
    crate::applets::false_::FALSE,
    crate::applets::echo::ECHO,
    crate::applets::cat::CAT,
    crate::applets::pwd::PWD,
    crate::applets::uname::UNAME,
    crate::applets::init::INIT,
    crate::applets::shell::SHELL,
    crate::applets::ls::LS,
    crate::applets::cp::CP,
    crate::applets::mv::MV,
    crate::applets::rm::RM,
    crate::applets::mkdir::MKDIR,
    crate::applets::touch::TOUCH,
    crate::applets::shutdown::SHUTDOWN,
    crate::applets::reboot::REBOOT,
    crate::applets::head::HEAD,
    crate::applets::tail::TAIL,
    crate::applets::wc::WC,
    crate::applets::grep::GREP,
    crate::applets::ln::LN,
    crate::applets::date::DATE,
    crate::applets::sleep::SLEEP,
    crate::applets::env::ENV,
    crate::applets::printf::PRINTF,
    crate::applets::basename::BASENAME,
    crate::applets::dirname::DIRNAME,
    crate::applets::status::STATUS,
];
