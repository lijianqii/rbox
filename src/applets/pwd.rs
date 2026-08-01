//! `pwd` - 输出当前工作目录。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Pwd;
pub static PWD: &Pwd = &Pwd;

impl Applet for Pwd {
    fn name(&self) -> &'static str {
        "pwd"
    }
    fn help(&self) -> &'static str {
        "pwd - print working directory"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        match std::env::current_dir() {
            Ok(p) => {
                println!("{}", p.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("pwd: {}", e);
                ExitCode::FAILURE
            }
        }
    }
}
