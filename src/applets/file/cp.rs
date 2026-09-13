//! `cp` - 复制文件。
//!
//! 用法：cp SOURCE DEST
//!       cp SOURCE... DIRECTORY
//!       cp -r SOURCE... DIRECTORY
//! `-r`/`-R` 递归复制目录（符号链接按内容复制？否：按链接本身复制）。

use crate::applet::Applet;
use crate::applets::file::util::{copy_recursive, is_dir, resolve_dest};
use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub struct Cp;
pub static CP: &Cp = &Cp;

impl Applet for Cp {
    fn name(&self) -> &'static str {
        "cp"
    }
    fn help(&self) -> &'static str {
        "cp [-r] SOURCE DEST | cp SOURCE... DIR - copy files (recursive with -r)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut recursive = false;
        let mut end_of_options = false;
        let mut files: Vec<&str> = Vec::new();
        for a in args {
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        continue;
                    }
                    "-r" | "-R" | "--recursive" => {
                        recursive = true;
                        continue;
                    }
                    _ => {}
                }
            }
            files.push(a.as_str());
        }
        if files.len() < 2 {
            eprintln!("cp: missing operand");
            return ExitCode::FAILURE;
        }

        let dest = files[files.len() - 1];
        let dest_is_dir = is_dir(dest);

        let mut had_error = false;

        if files.len() == 2 {
            // 单源
            if let Err(e) = copy_one(files[0], dest, dest_is_dir, recursive) {
                eprintln!("cp: {}: {}", files[0], e);
                had_error = true;
            }
        } else {
            // 多源：目标必须是目录
            if !dest_is_dir {
                eprintln!("cp: target '{}' is not a directory", dest);
                return ExitCode::FAILURE;
            }
            for src in &files[..files.len() - 1] {
                if let Err(e) = copy_one(src, dest, true, recursive) {
                    eprintln!("cp: {}: {}", src, e);
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

fn copy_one(src: &str, dest: &str, dest_is_dir: bool, recursive: bool) -> io::Result<()> {
    let src_meta = fs::symlink_metadata(src)?;
    let dest_path = if dest_is_dir {
        resolve_dest(src, dest)?
    } else {
        std::path::Path::new(dest).to_path_buf()
    };
    if src_meta.is_dir() {
        if !recursive {
            return Err(io::Error::other("omitting directory (use -r)"));
        }
        return copy_recursive(src, &dest_path);
    }
    // 符号链接：复制链接本身（与 cp -r 一致，不跟随）
    if src_meta.file_type().is_symlink() {
        let target = fs::read_link(src)?;
        let _ = fs::remove_file(&dest_path);
        return std::os::unix::fs::symlink(target, &dest_path);
    }

    let mut f_in = fs::File::open(src)?;
    let mut f_out = fs::File::create(&dest_path)?;

    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f_in.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f_out.write_all(&buf[..n])?;
    }

    // 尝试保留权限
    let _ = fs::set_permissions(&dest_path, fs::metadata(src)?.permissions());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn tmpdir() -> String {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = format!("/tmp/rbox_cp_test_{}_{}", std::process::id(), n);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(CP.name(), "cp");
        assert!(CP.help().contains("copy"));
    }

    #[test]
    fn cp_single_file() {
        let dir = tmpdir();
        let src = format!("{}/src", dir);
        let dst = format!("{}/dst", dir);
        fs::write(&src, "hello cp").unwrap();
        let args = vec![src.clone(), dst.clone()];
        let _ = CP.run(&args);
        assert_eq!(fs::read_to_string(&dst).unwrap(), "hello cp");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cp_to_dir() {
        let dir = tmpdir();
        let src = format!("{}/src", dir);
        let dstdir = format!("{}/dstdir", dir);
        fs::write(&src, "hello").unwrap();
        fs::create_dir(&dstdir).unwrap();
        let args = vec![src.clone(), dstdir.clone()];
        let _ = CP.run(&args);
        assert_eq!(
            fs::read_to_string(format!("{}/src", dstdir)).unwrap(),
            "hello"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cp_missing_operand() {
        let _ = CP.run(&[]);
        // Should not panic
    }

    #[test]
    fn cp_nonexistent_src() {
        let dir = tmpdir();
        let args = vec!["/nonexistent_src".to_string(), format!("{}/dst", dir)];
        let _ = CP.run(&args);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cp_dir_requires_recursive() {
        let dir = tmpdir();
        let srcdir = format!("{}/srcdir", dir);
        fs::create_dir_all(&srcdir).unwrap();
        fs::write(format!("{}/a.txt", srcdir), "a").unwrap();
        // 无 -r：失败
        let _ = CP.run(&[srcdir.clone(), format!("{}/dst", dir)]);
        assert!(!std::path::Path::new(&format!("{}/dst", dir)).exists());
        // -r：成功
        let _ = CP.run(&["-r".to_string(), srcdir.clone(), format!("{}/dst", dir)]);
        assert_eq!(
            fs::read_to_string(format!("{}/dst/a.txt", dir)).unwrap(),
            "a"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cp_recursive_into_existing_dir() {
        let dir = tmpdir();
        let srcdir = format!("{}/src", dir);
        let dstdir = format!("{}/dstdir", dir);
        fs::create_dir_all(&srcdir).unwrap();
        fs::create_dir_all(&dstdir).unwrap();
        fs::write(format!("{}/f.txt", srcdir), "x").unwrap();
        let _ = CP.run(&["-r".to_string(), srcdir.clone(), dstdir.clone()]);
        assert_eq!(
            fs::read_to_string(format!("{}/src/f.txt", dstdir)).unwrap(),
            "x"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
