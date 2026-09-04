//! Applet trait 与全局注册表。
//!
//! 每个 applet 实现 [`Applet`] trait 并提供静态实例，用 [`declare_applets!`] 宏登记到 [`APPLETS`]。
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

/// 声明所有 applet 注册表。
///
/// 用法：传入模块路径到静态实例的路径，宏展开为 `&[&dyn Applet]` 切片。
/// 新增 applet 时只需在此处添加一行。
macro_rules! declare_applets {
    ($($path:path),* $(,)?) => {
        pub static APPLETS: &[&dyn Applet] = &[
            $($path),*
        ];
    };
}

declare_applets! {
    crate::applets::sys::true_::TRUE,
    crate::applets::sys::false_::FALSE,
    crate::applets::text::echo::ECHO,
    crate::applets::file::cat::CAT,
    crate::applets::sys::pwd::PWD,
    crate::applets::sys::uname::UNAME,
    crate::applets::core::init::INIT,
    crate::applets::core::shell::SHELL,
    crate::applets::core::rgetty::GETTY,
    crate::applets::core::rlogin::LOGIN,
    crate::applets::file::ls::LS,
    crate::applets::file::cp::CP,
    crate::applets::file::mv::MV,
    crate::applets::file::rm::RM,
    crate::applets::file::mkdir::MKDIR,
    crate::applets::file::touch::TOUCH,
    crate::applets::core::shutdown::SHUTDOWN,
    crate::applets::core::reboot::REBOOT,
    crate::applets::text::head::HEAD,
    crate::applets::text::tail::TAIL,
    crate::applets::text::wc::WC,
    crate::applets::text::grep::GREP,
    crate::applets::file::ln::LN,
    crate::applets::sys::date::DATE,
    crate::applets::sys::sleep::SLEEP,
    crate::applets::sys::meminfo::MEMINFO,
    crate::applets::sys::processes::PROCESSES,
    crate::applets::sys::env::ENV,
    crate::applets::text::printf::PRINTF,
    crate::applets::text::basename::BASENAME,
    crate::applets::text::dirname::DIRNAME,
    crate::applets::core::status::STATUS,
    crate::applets::core::rservice::RSERVICE,
}
