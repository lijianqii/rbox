//! `mv` - 移动/重命名文件。
//!
//! 用法：mv SOURCE DEST
//!       mv SOURCE... DIRECTORY

use crate::applet::Applet;
use crate::applets::file::util::{copy_recursive, is_dir, remove_recursive, resolve_dest};
use std::fs;
use std::io;
use std::process::ExitCode;

pub struct Mv;
pub static MV: &Mv = &Mv;

impl Applet for Mv {
    fn name(&self) -> &'static str {
        "mv"
    }
    fn help(&self) -> &'static str {
        "mv SOURCE DEST | mv SOURCE... DIR - move/rename files"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let files: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if files.len() < 2 {
            eprintln!("mv: missing operand");
            return ExitCode::FAILURE;
        }

        let dest = files[files.len() - 1];
        let dest_is_dir = is_dir(dest);

        let mut had_error = false;

        if files.len() == 2 {
            if let Err(e) = move_one(files[0], dest, dest_is_dir) {
                eprintln!("mv: {}: {}", files[0], e);
                had_error = true;
            }
        } else {
            if !dest_is_dir {
                eprintln!("mv: target '{}' is not a directory", dest);
                return ExitCode::FAILURE;
            }
            for src in &files[..files.len() - 1] {
                if let Err(e) = move_one(src, dest, true) {
                    eprintln!("mv: {}: {}", src, e);
                    had_error = true;
                }
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
        let dir = format!("/tmp/rbox_mv_test_{}_{}", std::process::id(), n);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(MV.name(), "mv");
        assert!(MV.help().contains("move"));
    }

    #[test]
    fn mv_rename_file() {
        let dir = tmpdir();
        let src = format!("{}/src", dir);
        let dst = format!("{}/dst", dir);
        fs::write(&src, "hello mv").unwrap();
        let args = vec![src.clone(), dst.clone()];
        let _ = MV.run(&args);
        assert_eq!(fs::read_to_string(&dst).unwrap(), "hello mv");
        assert!(!Path::new(&src).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mv_to_dir() {
        let dir = tmpdir();
        let src = format!("{}/src", dir);
        let dstdir = format!("{}/dstdir", dir);
        fs::write(&src, "hello").unwrap();
        fs::create_dir(&dstdir).unwrap();
        let args = vec![src.clone(), dstdir.clone()];
        let _ = MV.run(&args);
        assert_eq!(
            fs::read_to_string(format!("{}/src", dstdir)).unwrap(),
            "hello"
        );
        assert!(!Path::new(&src).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mv_missing_operand() {
        let _ = MV.run(&[]);
    }

    #[test]
    fn mv_nonexistent_src() {
        let dir = tmpdir();
        let args = vec!["/nonexistent_src".to_string(), format!("{}/dst", dir)];
        let _ = MV.run(&args);
        let _ = fs::remove_dir_all(&dir);
    }
}

fn move_one(src: &str, dest: &str, dest_is_dir: bool) -> io::Result<()> {
    let dest_path = if dest_is_dir {
        resolve_dest(src, dest)?
    } else {
        std::path::Path::new(dest).to_path_buf()
    };

    // 先尝试 rename（同文件系统下 O(1)）
    match fs::rename(src, &dest_path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // 跨文件系统：先 cp 再 rm
            copy_recursive(src, &dest_path)?;
            remove_recursive(src)?;
            Ok(())
        }
    }
}
