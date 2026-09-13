//! `su` - 切换用户并启动 shell。
//!
//! 用法：su [user]
//! 非 root 切换需输入目标用户密码（复用 rlogin 的 shadow/crypt 校验）。

use crate::applet::Applet;
use crate::applets::core::rlogin::{authenticate, parse_passwd, read_password};
use std::ffi::CString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::ExitCode;

pub struct Su;
pub static SU: &Su = &Su;

impl Applet for Su {
    fn name(&self) -> &'static str {
        "su"
    }
    fn help(&self) -> &'static str {
        "su [user] - switch user and start a shell"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let cfg = crate::config::load();
        let user = args.first().map(String::as_str).unwrap_or("root");
        let uid = unsafe { libc::getuid() };

        // 查找目标用户
        let content = match std::fs::read_to_string(&cfg.paths.passwd) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("su: cannot read {}: {}", cfg.paths.passwd, e);
                return ExitCode::FAILURE;
            }
        };
        let entries = parse_passwd(&content);
        let Some(entry) = entries.iter().find(|e| e.name == user).cloned() else {
            eprintln!("su: user {} does not exist", user);
            return ExitCode::FAILURE;
        };

        // 非 root：验证密码
        if uid != 0 {
            let _ = write!(std::io::stdout(), "{}", cfg.login.password_prompt);
            let _ = std::io::stdout().flush();
            let Some(password) = read_password(Some(60)) else {
                eprintln!("\nsu: authentication failure");
                return ExitCode::FAILURE;
            };
            println!();
            if authenticate(user, &password).is_none() {
                eprintln!("su: authentication failure");
                return ExitCode::FAILURE;
            }
        }

        // 降权
        let name_c = match CString::new(entry.name.as_str()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("su: invalid user name");
                return ExitCode::FAILURE;
            }
        };
        unsafe {
            if libc::initgroups(name_c.as_ptr(), entry.gid as libc::gid_t) != 0 {
                eprintln!("su: initgroups failed: {}", std::io::Error::last_os_error());
                return ExitCode::FAILURE;
            }
            if libc::setgid(entry.gid as libc::gid_t) != 0 {
                eprintln!("su: setgid failed: {}", std::io::Error::last_os_error());
                return ExitCode::FAILURE;
            }
            if libc::setuid(entry.uid as libc::uid_t) != 0 {
                eprintln!("su: setuid failed: {}", std::io::Error::last_os_error());
                return ExitCode::FAILURE;
            }
        }

        let home = if std::env::set_current_dir(&entry.home).is_ok() {
            entry.home.clone()
        } else {
            "/".to_string()
        };
        let shell = if entry.shell.is_empty() {
            cfg.login.shell.clone()
        } else {
            entry.shell.clone()
        };
        unsafe {
            std::env::set_var("USER", &entry.name);
            std::env::set_var("LOGNAME", &entry.name);
            std::env::set_var("HOME", &home);
            std::env::set_var("SHELL", &shell);
        }
        let e = std::process::Command::new(&shell).exec();
        eprintln!("su: cannot start {}: {}", shell, e);
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(SU.name(), "su");
        assert!(SU.help().contains("switch"));
    }

    #[test]
    fn unknown_user_fails() {
        assert_eq!(SU.run(&["no_such_user_xyz".to_string()]), ExitCode::FAILURE);
    }
}
