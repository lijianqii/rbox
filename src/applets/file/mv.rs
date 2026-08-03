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
