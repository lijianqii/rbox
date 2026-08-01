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
