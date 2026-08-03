//! `basename` - 取文件名部分。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Basename;
pub static BASENAME: &Basename = &Basename;

impl Applet for Basename {
    fn name(&self) -> &'static str {
        "basename"
    }
    fn help(&self) -> &'static str {
        "basename PATH [SUFFIX] - strip directory and suffix"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("basename: missing operand");
            return ExitCode::from(1);
        }
        let suffix = args.get(1).map(|s| s.as_str());
        println!("{}", basename(&args[0], suffix));
        ExitCode::SUCCESS
    }
}

/// 提取路径的 basename（文件名部分）。
fn basename(path: &str, suffix: Option<&str>) -> String {
    let mut name = path.rsplit('/').next().unwrap_or(path).to_string();
    if name.is_empty() {
        let trimmed = path.trim_end_matches('/');
        name = trimmed.rsplit('/').next().unwrap_or(trimmed).to_string();
    }
    if let Some(sfx) = suffix
        && name.ends_with(sfx)
        && name.len() > sfx.len()
    {
        name.truncate(name.len() - sfx.len());
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_path() {
        assert_eq!(basename("/usr/bin/file", None), "file");
    }

    #[test]
    fn no_directory() {
        assert_eq!(basename("file.txt", None), "file.txt");
    }

    #[test]
    fn trailing_slash() {
        assert_eq!(basename("/usr/bin/", None), "bin");
    }

    #[test]
    fn root() {
        assert_eq!(basename("/", None), "");
    }

    #[test]
    fn with_suffix() {
        assert_eq!(basename("/path/file.txt", Some(".txt")), "file");
    }

    #[test]
    fn suffix_not_present() {
        assert_eq!(basename("file.txt", Some(".rs")), "file.txt");
    }

    #[test]
    fn suffix_equal_to_name() {
        // suffix equals name -> no removal (len > suffix.len() check)
        assert_eq!(basename("file", Some("file")), "file");
    }
}
