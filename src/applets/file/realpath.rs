//! `realpath` - 输出规范化绝对路径。
//!
//! 用法：realpath PATH...

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Realpath;
pub static REALPATH: &Realpath = &Realpath;

impl Applet for Realpath {
    fn name(&self) -> &'static str {
        "realpath"
    }
    fn help(&self) -> &'static str {
        "realpath PATH... - print resolved absolute path"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("realpath: missing operand");
            return ExitCode::FAILURE;
        }
        let mut had_error = false;
        for p in args {
            match std::fs::canonicalize(p) {
                Ok(path) => println!("{}", path.display()),
                Err(e) => {
                    eprintln!("realpath: {}: {}", p, e);
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
        assert_eq!(REALPATH.name(), "realpath");
        assert!(REALPATH.help().contains("absolute"));
    }

    #[test]
    fn resolves_dot_segments() {
        let path = format!("{}/.", std::env::temp_dir().display());
        assert_eq!(REALPATH.run(&[path]), ExitCode::SUCCESS);
    }

    #[test]
    fn missing_fails() {
        assert_eq!(
            REALPATH.run(&["/nonexistent_realpath_xyz".to_string()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn no_args_fails() {
        assert_eq!(REALPATH.run(&[]), ExitCode::FAILURE);
    }
}
