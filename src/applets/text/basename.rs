//! `basename` - 取文件名部分。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Basename;
pub static BASENAME: &Basename = &Basename;

impl Applet for Basename {
    fn name(&self) -> &'static str { "basename" }
    fn help(&self) -> &'static str { "basename PATH [SUFFIX] - strip directory and suffix" }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("basename: missing operand");
            return ExitCode::from(1);
        }
        let path = &args[0];
        let mut name = path.rsplit('/').next().unwrap_or(path).to_string();
        // 处理末尾的 /
        if name.is_empty() {
            // 路径以 / 结尾，取前一段
            let trimmed = path.trim_end_matches('/');
            name = trimmed.rsplit('/').next().unwrap_or(trimmed).to_string();
        }
        // 去除后缀
        if args.len() >= 2 {
            let suffix = &args[1];
            if name.ends_with(suffix) && name.len() > suffix.len() {
                name.truncate(name.len() - suffix.len());
            }
        }
        println!("{}", name);
        ExitCode::SUCCESS
    }
}
