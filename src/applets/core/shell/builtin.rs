//! 内置命令：cd、exit、export、unset、pwd、history。

use super::types::SimpleCmd;

/// 内置命令执行结果。
pub enum BuiltinResult {
    /// `exit N`：退出 shell。
    Exit,
    /// 内置命令执行完成，继续下一行。
    Done,
    /// 不是内置命令。
    NotBuiltin,
}

/// 尝试执行内置命令。
pub fn try_builtin(cmd: &SimpleCmd, last_rc: &mut i32, history: &[String]) -> BuiltinResult {
    if cmd.argv.is_empty() {
        return BuiltinResult::Done;
    }
    match cmd.argv[0].as_str() {
        "exit" => {
            let code = cmd
                .argv
                .get(1)
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(*last_rc as u8);
            *last_rc = code as i32;
            BuiltinResult::Exit
        }
        "cd" => {
            let target = cmd
                .argv
                .get(1)
                .cloned()
                .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));
            match std::env::set_current_dir(&target) {
                Ok(()) => *last_rc = 0,
                Err(e) => {
                    eprintln!("cd: {}: {}", target, e);
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "pwd" => {
            match std::env::current_dir() {
                Ok(p) => println!("{}", p.display()),
                Err(e) => {
                    eprintln!("pwd: {}", e);
                    *last_rc = 1;
                }
            }
            BuiltinResult::Done
        }
        "export" => {
            for arg in &cmd.argv[1..] {
                if let Some(eq) = arg.find('=') {
                    let (k, v) = arg.split_at(eq);
                    // SAFETY: single-threaded shell
                    unsafe {
                        std::env::set_var(k, &v[1..]);
                    }
                }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "unset" => {
            for arg in &cmd.argv[1..] {
                // SAFETY: single-threaded shell
                unsafe {
                    std::env::remove_var(arg);
                }
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        "history" => {
            for (i, h) in history.iter().enumerate() {
                println!("  {}  {}", i + 1, h);
            }
            *last_rc = 0;
            BuiltinResult::Done
        }
        _ => BuiltinResult::NotBuiltin,
    }
}
