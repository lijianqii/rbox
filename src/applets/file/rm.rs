//! `rm` - 删除文件或目录。
//!
//! 选项：
//!   -r, -R   递归删除目录
//!   -f       强制（忽略不存在的文件，不报错）

use crate::applet::Applet;
use crate::applets::file::util::remove_recursive;
use std::fs;
use std::io;
use std::process::ExitCode;

pub struct Rm;
pub static RM: &Rm = &Rm;

impl Applet for Rm {
    fn name(&self) -> &'static str {
        "rm"
    }
    fn help(&self) -> &'static str {
        "rm [-r] [-f] FILES... - remove files or directories"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut recursive = false;
        let mut force = false;
        let mut targets: Vec<&str> = Vec::new();

        for a in args {
            if a.starts_with('-') && a.len() > 1 && a != "-" {
                for c in a[1..].chars() {
                    match c {
                        'r' | 'R' => recursive = true,
                        'f' => force = true,
                        _ => {}
                    }
                }
            } else {
                targets.push(a);
            }
        }

        if targets.is_empty() {
            if !force {
                eprintln!("rm: missing operand");
                return ExitCode::FAILURE;
            }
            return ExitCode::SUCCESS;
        }

        let mut had_error = false;
        for t in &targets {
            if let Err(e) = remove_one(t, recursive, force) {
                if !force {
                    eprintln!("rm: {}: {}", t, e);
                }
                had_error = true;
            }
        }

        if had_error && !force {
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
        let dir = format!("/tmp/rbox_rm_test_{}_{}", std::process::id(), n);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(RM.name(), "rm");
        assert!(RM.help().contains("remove"));
    }

    #[test]
    fn rm_file() {
        let dir = tmpdir();
        let f = format!("{}/file", dir);
        fs::write(&f, "x").unwrap();
        let args = vec![f.clone()];
        let _ = RM.run(&args);
        assert!(!Path::new(&f).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rm_recursive() {
        let dir = tmpdir();
        let sub = format!("{}/sub", dir);
        fs::create_dir(&sub).unwrap();
        fs::write(format!("{}/sub/a", dir), "a").unwrap();
        fs::write(format!("{}/sub/b", dir), "b").unwrap();
        let args = vec!["-r".to_string(), sub.clone()];
        let _ = RM.run(&args);
        assert!(!Path::new(&sub).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rm_force_nonexistent() {
        let args = vec!["-f".to_string(), "/nonexistent_xyz".to_string()];
        let _ = RM.run(&args);
        // Should not panic
    }

    #[test]
    fn rm_dir_without_r_fails() {
        let dir = tmpdir();
        let sub = format!("{}/sub", dir);
        fs::create_dir(&sub).unwrap();
        let args = vec![sub.clone()];
        let _ = RM.run(&args);
        // Without -r, should fail but not panic
        let _ = fs::remove_dir_all(&dir);
    }
}

fn remove_one(path: &str, recursive: bool, force: bool) -> io::Result<()> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => {
            if force {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such file or directory",
            ));
        }
    };

    if meta.is_dir() {
        if !recursive {
            return Err(io::Error::other("is a directory"));
        }
        remove_recursive(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
