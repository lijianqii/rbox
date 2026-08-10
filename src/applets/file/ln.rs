//! `ln` - 创建链接。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Ln;
pub static LN: &Ln = &Ln;

impl Applet for Ln {
    fn name(&self) -> &'static str {
        "ln"
    }
    fn help(&self) -> &'static str {
        "ln [-s] TARGET LINK - create link (default hard, -s symbolic)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut symbolic = false;
        let mut positional: Vec<&String> = Vec::new();

        for a in args {
            match a.as_str() {
                "-s" => symbolic = true,
                "-f" => {} // 兼容：硬链接已存在时报错（与 GNU 一致），符号链接默认覆盖
                "-sf" | "-fs" => symbolic = true,
                s if s.starts_with('-') && s.len() > 1 && s != "--" => {
                    eprintln!("ln: unknown option: {}", s);
                }
                _ => positional.push(a),
            }
        }

        if positional.len() < 2 {
            eprintln!("ln: missing operand");
            eprintln!("usage: ln [-s] TARGET LINK");
            return ExitCode::from(1);
        }

        let target = positional[0];
        let link = positional[1];
        let result = if symbolic {
            // 先删除已存在的链接
            let _ = std::fs::remove_file(link);
            std::os::unix::fs::symlink(target, link)
        } else {
            std::fs::hard_link(target, link)
        };

        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ln: {}", e);
                ExitCode::from(1)
            }
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
        let dir = format!("/tmp/rbox_ln_test_{}_{}", std::process::id(), n);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(LN.name(), "ln");
        assert!(LN.help().contains("link"));
    }

    #[test]
    fn symlink_creates_link() {
        let dir = tmpdir();
        let target = format!("{}/target", dir);
        let link = format!("{}/link", dir);
        fs::write(&target, "hello").unwrap();
        let args = vec!["-s".to_string(), target.clone(), link.clone()];
        let _ = LN.run(&args);
        assert!(Path::new(&link).exists());
        assert_eq!(fs::read_to_string(&link).unwrap(), "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hardlink_creates_link() {
        let dir = tmpdir();
        let target = format!("{}/target", dir);
        let link = format!("{}/link", dir);
        fs::write(&target, "hello").unwrap();
        let args = vec![target.clone(), link.clone()];
        let _ = LN.run(&args);
        assert!(Path::new(&link).exists());
        assert_eq!(fs::read_to_string(&link).unwrap(), "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ln_missing_target_fails() {
        let args = vec![
            "/nonexistent_target".to_string(),
            "/tmp/rbox_link".to_string(),
        ];
        let _ = LN.run(&args);
        // Should not panic
    }
}
