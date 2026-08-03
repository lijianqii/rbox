//! `cp` - 复制文件。
//!
//! 用法：cp SOURCE DEST
//!       cp SOURCE... DIRECTORY
//! 不递归复制目录（保持简单）。

use crate::applet::Applet;
use crate::applets::file::util::{is_dir, resolve_dest};
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
        "cp SOURCE DEST | cp SOURCE... DIR - copy files"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let files: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if files.len() < 2 {
            eprintln!("cp: missing operand");
            return ExitCode::FAILURE;
        }

        let dest = files[files.len() - 1];
        let dest_is_dir = is_dir(dest);

        let mut had_error = false;

        if files.len() == 2 {
            // 单源
            if let Err(e) = copy_one(files[0], dest, dest_is_dir) {
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
                if let Err(e) = copy_one(src, dest, true) {
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

fn copy_one(src: &str, dest: &str, dest_is_dir: bool) -> io::Result<()> {
    let src_meta = fs::metadata(src)?;
    if src_meta.is_dir() {
        return Err(io::Error::other("omitting directory (not supported)"));
    }

    let dest_path = if dest_is_dir {
        resolve_dest(src, dest)?
    } else {
        std::path::Path::new(dest).to_path_buf()
    };

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
