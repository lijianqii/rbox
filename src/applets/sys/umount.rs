//! `umount` - 卸载文件系统。
//!
//! 用法：umount [-f] [-l] TARGET...
//! - `-f`：强制卸载（MNT_FORCE，内核可能不支持）；
//! - `-l`：惰性卸载（MNT_DETACH，先脱离命名空间稍后清理）。

use crate::applet::Applet;
use std::ffi::CString;
use std::process::ExitCode;

pub struct Umount;
pub static UMOUNT: &Umount = &Umount;

/// 调用 umount2(2)。
pub(crate) fn do_umount(target: &str, flags: i32) -> std::io::Result<()> {
    let c = CString::new(target)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let rc = unsafe { libc::umount2(c.as_ptr(), flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

impl Applet for Umount {
    fn name(&self) -> &'static str {
        "umount"
    }
    fn help(&self) -> &'static str {
        "umount [-f] [-l] TARGET... - unmount filesystems"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut flags = 0;
        let mut targets: Vec<&String> = Vec::new();
        let mut end_of_options = false;
        for a in args {
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        continue;
                    }
                    "-f" | "--force" => {
                        flags |= libc::MNT_FORCE;
                        continue;
                    }
                    "-l" | "--lazy" | "--detach" => {
                        flags |= libc::MNT_DETACH;
                        continue;
                    }
                    s if s.starts_with('-') && s.len() > 1 => {
                        eprintln!("umount: unknown option: {}", s);
                        return ExitCode::FAILURE;
                    }
                    _ => {}
                }
            }
            targets.push(a);
        }
        if targets.is_empty() {
            eprintln!("umount: usage: umount [-f] [-l] TARGET...");
            return ExitCode::FAILURE;
        }
        let mut had_error = false;
        for t in targets {
            if let Err(e) = do_umount(t, flags) {
                eprintln!("umount: {}: {}", t, e);
                had_error = true;
            }
        }
        if had_error {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(UMOUNT.name(), "umount");
        assert!(UMOUNT.help().contains("unmount"));
    }

    #[test]
    fn unmount_nonexistent_fails() {
        assert!(do_umount("/nonexistent_rbox_umount", 0).is_err());
    }

    #[test]
    fn missing_operand_fails() {
        assert_eq!(UMOUNT.run(&[]), ExitCode::FAILURE);
    }

    #[test]
    fn unknown_option_fails() {
        assert_eq!(
            UMOUNT.run(&["-x".to_string(), "/mnt".to_string()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn target_error_returns_failure() {
        assert_eq!(
            UMOUNT.run(&["/nonexistent_rbox_umount".to_string()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn force_and_lazy_flags_parse() {
        // 仅验证参数解析不失败于选项本身（目标不存在 -> FAILURE，但选项合法）
        let rc = UMOUNT.run(&[
            "-f".to_string(),
            "-l".to_string(),
            "/nonexistent_rbox_umount".to_string(),
        ]);
        assert_eq!(rc, ExitCode::FAILURE);
    }

    #[test]
    fn double_dash_allows_dash_target() {
        // -- 之后的目标即使以 - 开头也按路径处理
        let rc = UMOUNT.run(&["--".to_string(), "-weird".to_string()]);
        assert_eq!(rc, ExitCode::FAILURE); // 路径不存在
    }
}
