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
            // bash 语义：退出码取低 8 位（exit 300 -> 44）；非数字保持 last_rc
            let code = cmd
                .argv
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .map(|c| c & 0xff)
                .unwrap_or(*last_rc & 0xff);
            *last_rc = code;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cmd(args: &[&str]) -> SimpleCmd {
        SimpleCmd {
            argv: args.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn exit_returns_exit() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["exit"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Exit));
    }

    #[test]
    fn exit_with_code() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["exit", "42"]), &mut rc, &[]);
        assert_eq!(rc, 42);
    }

    #[test]
    fn exit_default_last_rc() {
        let mut rc = 7;
        try_builtin(&make_cmd(&["exit"]), &mut rc, &[]);
        assert_eq!(rc, 7);
    }

    #[test]
    fn cd_sets_cwd() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["cd", "/tmp"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
        assert_eq!(std::env::current_dir().unwrap().to_string_lossy(), "/tmp");
    }

    #[test]
    fn cd_nonexistent_fails() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["cd", "/nonexistent_xyz"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 1);
    }

    #[test]
    fn pwd_prints_cwd() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["pwd"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn export_sets_var() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["export", "RBOX_TEST=123"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(std::env::var("RBOX_TEST").unwrap(), "123");
        unsafe {
            std::env::remove_var("RBOX_TEST");
        }
    }

    #[test]
    fn unset_removes_var() {
        unsafe {
            std::env::set_var("RBOX_TEST_UNSET", "val");
        }
        let mut rc = 0;
        try_builtin(&make_cmd(&["unset", "RBOX_TEST_UNSET"]), &mut rc, &[]);
        assert!(std::env::var("RBOX_TEST_UNSET").is_err());
    }

    #[test]
    fn history_lists_entries() {
        let mut rc = 0;
        let history = vec!["echo a".to_string(), "echo b".to_string()];
        let result = try_builtin(&make_cmd(&["history"]), &mut rc, &history);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn unknown_returns_not_builtin() {
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["nonexistent_cmd"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::NotBuiltin));
    }

    #[test]
    fn empty_argv_returns_done() {
        let mut rc = 0;
        let result = try_builtin(&SimpleCmd::default(), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
    }

    // ─── cd 边界 ───────────────────────────────

    #[test]
    fn cd_to_root() {
        let mut rc = 0;
        try_builtin(&make_cmd(&["cd", "/"]), &mut rc, &[]);
        assert_eq!(rc, 0);
    }

    #[test]
    fn cd_no_arg_goes_home() {
        unsafe {
            std::env::set_var("HOME", "/tmp");
        }
        let mut rc = 0;
        try_builtin(&make_cmd(&["cd"]), &mut rc, &[]);
        assert_eq!(rc, 0);
        assert_eq!(std::env::current_dir().unwrap().to_string_lossy(), "/tmp");
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    // ─── export 边界 ───────────────────────────

    #[test]
    fn export_without_eq_sign() {
        // export VAR (no =) -> does nothing, just marks as exported
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["export", "RBOX_EMPTY"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    #[test]
    fn export_no_arg() {
        // export with no args -> Done (lists all vars, but we just check it doesn't fail)
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["export"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
    }

    // ─── unset 边界 ────────────────────────────

    #[test]
    fn unset_nonexistent_var() {
        // unset nonexistent -> no error
        let mut rc = 0;
        let result = try_builtin(&make_cmd(&["unset", "RBOX_NOEXIST"]), &mut rc, &[]);
        assert!(matches!(result, BuiltinResult::Done));
        assert_eq!(rc, 0);
    }

    // ─── exit with invalid code ────────────────

    #[test]
    fn exit_with_non_numeric_code() {
        let mut rc = 5;
        try_builtin(&make_cmd(&["exit", "abc"]), &mut rc, &[]);
        // Non-numeric exit code -> keeps last rc
        assert_eq!(rc, 5);
    }

    #[test]
    fn exit_truncates_to_8_bits() {
        // bash 语义：exit 300 -> 300 & 0xff = 44
        let mut rc = 0;
        try_builtin(&make_cmd(&["exit", "300"]), &mut rc, &[]);
        assert_eq!(rc, 44);
        let mut rc = 0;
        try_builtin(&make_cmd(&["exit", "-1"]), &mut rc, &[]);
        assert_eq!(rc, 255);
    }
}
