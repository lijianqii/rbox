//! `dirname` - 取目录部分。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Dirname;
pub static DIRNAME: &Dirname = &Dirname;

impl Applet for Dirname {
    fn name(&self) -> &'static str { "dirname" }
    fn help(&self) -> &'static str { "dirname PATH - strip last component" }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("dirname: missing operand");
            return ExitCode::from(1);
        }
        let path = &args[0];
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            // 路径全是 /
            println!("/");
            return ExitCode::SUCCESS;
        }
        match trimmed.rfind('/') {
            Some(0) => println!("/"),
            Some(pos) => println!("{}", &trimmed[..pos]),
            None => println!("."),
        }
        ExitCode::SUCCESS
    }
}
