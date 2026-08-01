//! `tail` - 输出文件后 N 行。
use crate::applet::Applet;
use crate::applets::text::util::{each_input, parse_n_files};
use std::io::Write;
use std::process::ExitCode;

pub struct Tail;
pub static TAIL: &Tail = &Tail;

impl Applet for Tail {
    fn name(&self) -> &'static str {
        "tail"
    }
    fn help(&self) -> &'static str {
        "tail [-n N] [file] - print last N lines (default 10)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (n, files) = parse_n_files(args, "tail");
        let mut out = std::io::stdout().lock();
        each_input(&files, "tail", &mut out, |content, out| {
            let lines: Vec<&str> = content.lines().collect();
            let start = if lines.len() > n { lines.len() - n } else { 0 };
            for line in &lines[start..] {
                let _ = writeln!(out, "{}", line);
            }
        });
        ExitCode::SUCCESS
    }
}
