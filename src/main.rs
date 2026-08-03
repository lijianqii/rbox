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

use crate::applet::Applet;
use std::process::ExitCode;

fn main() -> ExitCode {
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
        if app_args.first().is_some_and(|a| a == "--help" || a == "-h")
            && let Some(app) = applet_for(&basename)
        {
            eprintln!("{}", app.help());
            return ExitCode::SUCCESS;
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
    if args.first().is_some_and(|a| a == "--help" || a == "-h")
        && let Some(app) = applet_for(cmd)
    {
        eprintln!("{}", app.help());
        return ExitCode::SUCCESS;
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
