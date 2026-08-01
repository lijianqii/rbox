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
            return Err(io::Error::new(io::ErrorKind::Other, "is a directory"));
        }
        remove_recursive(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
