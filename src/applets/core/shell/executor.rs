//! 执行器：执行命令列表、管道、重定向、外部命令查找。

use super::builtin::{try_builtin, BuiltinResult};
use super::expander::expand_pipeline;
use super::expander::expand_history;
use super::parser::build_command_list;
use super::tokenizer::tokenize;
use super::types::*;
use std::process::{Child, Command, Stdio};

/// 执行一行命令。返回退出码。exit_fn 用于 `exit` 内置命令。
pub fn execute_line<F>(line: &str, last_rc: &mut i32, history: &[String], exit_fn: F) -> i32
where
    F: Fn(i32) -> (),
{
    // 历史扩展：!! -> 上一条命令，!n -> 第 n 条，!$ -> 上一条命令的最后一个参数
    let expanded_line = expand_history(line, history);
    let line = expanded_line.as_str();

    let tokens = tokenize(line);
    let cmd_list = match build_command_list(&tokens) {
        Ok(cl) => cl,
        Err(e) => {
            eprintln!("shell: {}", e);
            *last_rc = 2;
            return *last_rc;
        }
    };

    for seg in &cmd_list.segments {
        match seg.connector {
            Connector::Start | Connector::Sequential => {}
            Connector::AndIf => {
                if *last_rc != 0 {
                    continue;
                }
            }
            Connector::OrIf => {
                if *last_rc == 0 {
                    continue;
                }
            }
        }

        let expanded = match expand_pipeline(&seg.pipeline, *last_rc) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("shell: {}", e);
                *last_rc = 1;
                continue;
            }
        };

        if expanded.cmds.is_empty() {
            continue;
        }

        if expanded.cmds.len() == 1 && !expanded.background {
            match try_builtin(&expanded.cmds[0], last_rc, history) {
                BuiltinResult::Exit => {
                    exit_fn(*last_rc);
                }
                BuiltinResult::Done => {
                    continue;
                }
                BuiltinResult::NotBuiltin => {}
            }
        }

        *last_rc = execute_pipeline(&expanded);
    }

    *last_rc
}

/// 执行一条管道（可能含多条 SimpleCmd）。
fn execute_pipeline(pipeline: &Pipeline) -> i32 {
    if pipeline.cmds.is_empty() {
        return 0;
    }

    let mut children: Vec<Child> = Vec::new();
    let ncmds = pipeline.cmds.len();

    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        if cmd.argv.is_empty() {
            continue;
        }

        let (program, extra_args) = resolve_command(&cmd.argv[0]);

        let mut command = Command::new(program);
        command.args(&extra_args);
        command.args(&cmd.argv[1..]);

        // stdin
        if let Some(ref f) = cmd.stdin_file {
            match std::fs::File::open(f) {
                Ok(file) => {
                    command.stdin(Stdio::from(file));
                }
                Err(e) => {
                    eprintln!("shell: {}: {}", f, e);
                    return 1;
                }
            }
        } else if i > 0 {
            // 管道中间命令：stdin 来自前一个命令的 pipe（在下面处理）
        }

        // stdout
        if let Some(ref f) = cmd.stdout_file {
            let mut opts = std::fs::OpenOptions::new();
            let opts = if cmd.append {
                opts.create(true).append(true).open(f)
            } else {
                opts.create(true).write(true).truncate(true).open(f)
            };
            match opts {
                Ok(file) => {
                    command.stdout(Stdio::from(file));
                }
                Err(e) => {
                    eprintln!("shell: {}: {}", f, e);
                    return 1;
                }
            }
        } else if i < ncmds - 1 {
            command.stdout(Stdio::piped());
        }

        // 管道中间命令的 stdin 来自前一个命令的 stdout
        if i > 0 && cmd.stdin_file.is_none() {
            if let Some(prev) = children.last_mut() {
                if let Some(stdout) = prev.stdout.take() {
                    command.stdin(Stdio::from(stdout));
                }
            }
        }

        match command.spawn() {
            Ok(child) => children.push(child),
            Err(e) => {
                eprintln!("shell: {}: {}", cmd.argv[0], e);
                return 127;
            }
        }
    }

    // 后台运行：不等待
    if pipeline.background {
        return 0;
    }

    // 等待所有子进程，返回最后一个的退出码
    let mut last_code = 0;
    for child in &mut children {
        match child.wait() {
            Ok(status) => last_code = status.code().unwrap_or(1),
            Err(_) => last_code = 1,
        }
    }
    last_code
}

/// 命令查找：含 `/` 按字面路径，否则在 PATH 下查找。
/// 查找失败时回退到 rbox 内置 applet（`rbox <cmd>`）。
fn resolve_command(cmd: &str) -> (String, Vec<String>) {
    if cmd.contains('/') {
        return (cmd.to_string(), Vec::new());
    }

    // 在 PATH 下查找
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            let full = format!("{}/{}", dir, cmd);
            if std::path::Path::new(&full).is_file() {
                return (full, Vec::new());
            }
        }
    }

    // 回退：rbox 内置 applet
    let rbox_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/bin/rbox".to_string());
    (rbox_path, vec![cmd.to_string()])
}
