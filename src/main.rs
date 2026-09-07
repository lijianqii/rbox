//! rbox - 一个类似 busybox 的多合一二进制。
//!
//! 分发逻辑：
//! - 取 argv[0] 的 basename。
//! - 若 basename 为 `rbox`，则用 argv[1] 作为子命令，argv[2..] 作为参数。
//! - 若 basename 是已注册 applet 名（如通过 symlink `ln -s rbox echo`），
//!   则直接以该 applet 执行，参数为 argv[1..]。
//! - 未命中则打印 usage。

mod applet;
mod applets;
mod config;

use crate::applet::Applet;
use std::process::ExitCode;

fn main() -> ExitCode {
    // PID 1 崩溃保护：panic → abort 会导致 PID 1 死亡（kernel panic），
    // 这里把 panic 信息写一行到 /dev/kmsg，便于事后从 dmesg/console 定位死因。
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("rbox panic: {}", info);
        if let Ok(mut kmsg) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
            use std::io::Write;
            let _ = kmsg.write_all(format!("\n{}\n", msg).as_bytes());
        }
        eprintln!("{}", msg);
    }));

    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.is_empty() {
        eprintln!("rbox: no argv[0]");
        return ExitCode::FAILURE;
    }

    let argv0 = &raw_args[0];
    let basename = std::path::Path::new(argv0)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.clone());

    // 根据分发方式确定"命令名"和"参数"。
    let (cmd, args): (&str, &[String]) = if basename == "rbox" {
        // subcommand 模式：rbox <applet> [args...]
        if raw_args.len() < 2 {
            return print_usage(false);
        }
        let sub = &raw_args[1];
        // 内置元命令
        match sub.as_str() {
            "--list" | "list" => return print_list(),
            "--help" | "-h" | "help" => return print_usage(true),
            "--version" | "-V" | "version" => return print_version(),
            _ => {}
        }
        (sub.as_str(), &raw_args[2..])
    } else {
        // argv[0] 分发模式：basename 即命令名（如 bin/echo -> rbox）
        let app_args = &raw_args[1..];
        // 拦截 --help/-h
        if let Some(code) = try_print_help(&basename, app_args) {
            return code;
        }
        return match applet_for(&basename) {
            Some(app) => app.run(app_args),
            None => {
                eprintln!("rbox: unknown command '{}'", basename);
                print_usage(false)
            }
        };
    };

    // subcommand 模式查找
    // 拦截 --help/-h：打印该 applet 的帮助信息
    if let Some(code) = try_print_help(cmd, args) {
        return code;
    }

    match applet_for(cmd) {
        Some(app) => app.run(args),
        None => {
            eprintln!("rbox: unknown command '{}'", cmd);
            print_usage(false)
        }
    }
}

/// 按命令名查找 applet。
fn applet_for(name: &str) -> Option<&'static dyn Applet> {
    applet::APPLETS.iter().find(|a| a.name() == name).copied()
}

/// 若参数首项为 `--help`/`-h` 且命令存在，打印帮助并返回退出码；否则返回 None。
fn try_print_help(name: &str, args: &[String]) -> Option<ExitCode> {
    if args.first().is_some_and(|a| a == "--help" || a == "-h")
        && let Some(app) = applet_for(name)
    {
        eprintln!("{}", app.help());
        Some(ExitCode::SUCCESS)
    } else {
        None
    }
}

/// 打印用法。`ok=true`（--help）返回成功，其余错误路径返回失败。
fn print_usage(ok: bool) -> ExitCode {
    eprintln!(
        "rbox v{} - a busybox-like multi-binary",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  rbox <applet> [args...]   run an applet");
    eprintln!("  <applet> [args...]         via symlink (argv[0] dispatch)");
    eprintln!("  rbox --list                list all applets");
    eprintln!("  rbox --version             show version");
    eprintln!();
    eprintln!("Applets ({}):", applet::APPLETS.len());
    for app in applet::APPLETS {
        let h = app.help();
        if h.is_empty() {
            eprintln!("  {}", app.name());
        } else {
            eprintln!("  {:12} {}", app.name(), h);
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_list() -> ExitCode {
    for app in applet::APPLETS {
        println!("{}", app.name());
    }
    ExitCode::SUCCESS
}

fn print_version() -> ExitCode {
    println!("rbox {}", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_nonempty_and_unique() {
        assert!(!applet::APPLETS.is_empty());
        let mut names: Vec<&str> = applet::APPLETS.iter().map(|a| a.name()).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "applet 名字必须唯一");
    }

    #[test]
    fn applet_for_finds_registered() {
        assert!(applet_for("echo").is_some());
        assert!(applet_for("cat").is_some());
        assert!(applet_for("rservice").is_some());
        assert_eq!(applet_for("echo").unwrap().name(), "echo");
    }

    #[test]
    fn applet_for_unknown_returns_none() {
        assert!(applet_for("ghost-command").is_none());
        assert!(applet_for("").is_none());
    }

    #[test]
    fn help_flag_returns_success_for_known_applet() {
        let args = vec!["--help".to_string()];
        assert!(try_print_help("echo", &args).is_some());
        let args = vec!["-h".to_string()];
        assert!(try_print_help("cat", &args).is_some());
    }

    #[test]
    fn help_flag_unknown_applet_returns_none() {
        // 未知命令的 --help 不该打印帮助，交给分发层报 unknown command
        let args = vec!["--help".to_string()];
        assert!(try_print_help("ghost", &args).is_none());
    }

    #[test]
    fn non_help_args_return_none() {
        let args = vec!["-n".to_string(), "3".to_string()];
        assert!(try_print_help("head", &args).is_none());
        let args: Vec<String> = vec![];
        assert!(try_print_help("echo", &args).is_none());
    }

    #[test]
    fn every_applet_has_help_text() {
        // 帮助文本非空且提及命令名（sh 的 help 以 "rbox shell" 开头，为特例）
        for app in applet::APPLETS {
            let h = app.help();
            assert!(!h.is_empty(), "{} 缺少 help", app.name());
            assert!(
                h.contains(app.name()),
                "{} 的 help 应提及命令名: {}",
                app.name(),
                h
            );
        }
    }
}
