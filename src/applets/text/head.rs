//! `head` - 输出文件前 N 行。
use crate::applet::Applet;
use crate::applets::text::util::{each_input, parse_n_files};
use std::io::Write;
use std::process::ExitCode;

pub struct Head;
pub static HEAD: &Head = &Head;

impl Applet for Head {
    fn name(&self) -> &'static str {
        "head"
    }
    fn help(&self) -> &'static str {
        "head [-n N] [file] - print first N lines (default 10)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (n, files) = parse_n_files(args, "head");
        let mut out = std::io::stdout().lock();
        each_input(&files, "head", &mut out, |content, out| {
            for line in content.lines().take(n) {
                let _ = writeln!(out, "{}", line);
            }
        });
        ExitCode::SUCCESS
    }
}
