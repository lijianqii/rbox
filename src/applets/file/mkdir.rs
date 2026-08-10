//! `mkdir` - 创建目录。
//!
//! 选项：
//!   -p   递归创建（不报错如果已存在）

use crate::applet::Applet;
use std::fs;
use std::process::ExitCode;

pub struct Mkdir;
pub static MKDIR: &Mkdir = &Mkdir;

impl Applet for Mkdir {
    fn name(&self) -> &'static str {
        "mkdir"
    }
    fn help(&self) -> &'static str {
        "mkdir [-p] DIRS... - create directories"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut parents = false;
        let mut dirs: Vec<&str> = Vec::new();

        for a in args {
            if a.starts_with('-') && a.len() > 1 {
                for c in a[1..].chars() {
                    if c == 'p' {
                        parents = true;
                    }
                }
            } else {
                dirs.push(a);
            }
        }

        if dirs.is_empty() {
            eprintln!("mkdir: missing operand");
            return ExitCode::FAILURE;
        }

        let mut had_error = false;
        for d in &dirs {
            let r = if parents {
                fs::create_dir_all(d)
            } else {
                fs::create_dir(d)
            };
            if let Err(e) = r {
                eprintln!("mkdir: {}: {}", d, e);
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
    use std::fs;
    use std::path::Path;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn tmpdir() -> String {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = format!("/tmp/rbox_mkdir_test_{}_{}", std::process::id(), n);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(MKDIR.name(), "mkdir");
        assert!(MKDIR.help().contains("directories"));
    }

    #[test]
    fn mkdir_simple() {
        let dir = tmpdir();
        let target = format!("{}/newdir", dir);
        let args = vec![target.clone()];
        let _ = MKDIR.run(&args);
        assert!(Path::new(&target).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mkdir_p_recursive() {
        let dir = tmpdir();
        let target = format!("{}/a/b/c", dir);
        let args = vec!["-p".to_string(), target.clone()];
        let _ = MKDIR.run(&args);
        assert!(Path::new(&target).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mkdir_existing_with_p() {
        let dir = tmpdir();
        let target = format!("{}/exists", dir);
        fs::create_dir(&target).unwrap();
        let args = vec!["-p".to_string(), target.clone()];
        let _ = MKDIR.run(&args);
        // Should succeed (not error) with -p
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mkdir_multiple_dirs() {
        let dir = tmpdir();
        let t1 = format!("{}/d1", dir);
        let t2 = format!("{}/d2", dir);
        let args = vec![t1.clone(), t2.clone()];
        let _ = MKDIR.run(&args);
        assert!(Path::new(&t1).exists());
        assert!(Path::new(&t2).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
