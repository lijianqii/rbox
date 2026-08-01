//! `mv` - 移动/重命名文件。
//!
//! 用法：mv SOURCE DEST
//!       mv SOURCE... DIRECTORY

use crate::applet::Applet;
use crate::applets::file::util::remove_recursive;
use std::fs;
use std::io;
use std::path::Path;
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
        let dest_meta = fs::metadata(dest);
        let dest_is_dir = dest_meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);

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

fn move_one(src: &str, dest: &str, dest_is_dir: bool) -> io::Result<()> {
    let dest_path = if dest_is_dir {
        let name = Path::new(src)
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "cannot determine filename"))?;
        Path::new(dest).join(name)
    } else {
        Path::new(dest).to_path_buf()
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

/// 简单递归复制（跨文件系统 mv 用）。
fn copy_recursive(src: &str, dest: &std::path::Path) -> io::Result<()> {
    let meta = fs::metadata(src)?;
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_recursive(
                &Path::new(src).join(&name).to_string_lossy(),
                &dest.join(&name),
            )?;
        }
    } else {
        let mut f_in = fs::File::open(src)?;
        let mut f_out = fs::File::create(dest)?;
        io::copy(&mut f_in, &mut f_out)?;
    }
    Ok(())
}
