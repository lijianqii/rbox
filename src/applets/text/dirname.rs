//! `dirname` - 取目录部分。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Dirname;
pub static DIRNAME: &Dirname = &Dirname;

impl Applet for Dirname {
    fn name(&self) -> &'static str {
        "dirname"
    }
    fn help(&self) -> &'static str {
        "dirname PATH - strip last component"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("dirname: missing operand");
            return ExitCode::from(1);
        }
        let path = &args[0];
        println!("{}", dirname(path));
        ExitCode::SUCCESS
    }
}

/// 提取路径的 dirname（目录部分）。
fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(pos) => trimmed[..pos].to_string(),
        None => ".".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_path() {
        assert_eq!(dirname("/usr/bin/file"), "/usr/bin");
    }

    #[test]
    fn no_directory() {
        assert_eq!(dirname("file"), ".");
    }

    #[test]
    fn trailing_slash() {
        assert_eq!(dirname("/usr/bin/"), "/usr");
    }

    #[test]
    fn root() {
        assert_eq!(dirname("/"), "/");
    }

    #[test]
    fn single_component() {
        assert_eq!(dirname("/file"), "/");
    }
}
