//! `readlink` - 读取符号链接目标 / 规范化路径。
//!
//! 用法：readlink [-f] [-n] PATH...
//! - `-f` 规范化（等价 realpath，要求存在）；`-n` 不输出末尾换行。

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Readlink;
pub static READLINK: &Readlink = &Readlink;

impl Applet for Readlink {
    fn name(&self) -> &'static str {
        "readlink"
    }
    fn help(&self) -> &'static str {
        "readlink [-f] [-n] PATH... - print symlink target or canonical path"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut canonical = false;
        let mut no_newline = false;
        let mut paths: Vec<&String> = Vec::new();
        let mut end_of_options = false;
        for a in args {
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        continue;
                    }
                    "-f" | "--canonicalize" => {
                        canonical = true;
                        continue;
                    }
                    "-n" | "--no-newline" => {
                        no_newline = true;
                        continue;
                    }
                    s if s.starts_with('-') && s.len() > 1 => {
                        eprintln!("readlink: unknown option: {}", s);
                        return ExitCode::FAILURE;
                    }
                    _ => {}
                }
            }
            paths.push(a);
        }
        if paths.is_empty() {
            eprintln!("readlink: missing operand");
            return ExitCode::FAILURE;
        }
        let mut had_error = false;
        for p in paths {
            let result = if canonical {
                std::fs::canonicalize(p).map(|pb| pb.to_string_lossy().into_owned())
            } else {
                std::fs::read_link(p).map(|pb| pb.to_string_lossy().into_owned())
            };
            match result {
                Ok(target) => {
                    if no_newline {
                        print!("{}", target);
                    } else {
                        println!("{}", target);
                    }
                }
                Err(e) => {
                    eprintln!("readlink: {}: {}", p, e);
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

    #[test]
    fn name_and_help() {
        assert_eq!(READLINK.name(), "readlink");
        assert!(READLINK.help().contains("symlink"));
    }

    #[test]
    fn canonicalize_existing() {
        let out = std::fs::canonicalize("/etc/hostname").unwrap();
        assert!(out.is_absolute());
        assert_eq!(
            READLINK.run(&["-f".to_string(), "/etc/hostname".to_string()]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn missing_target_fails() {
        assert_eq!(
            READLINK.run(&["/nonexistent_readlink_xyz".to_string()]),
            ExitCode::FAILURE
        );
    }
}
