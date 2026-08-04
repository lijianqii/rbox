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
        let ok = each_input(&files, "head", &mut out, |content, out| {
            for line in head_lines(content, n) {
                let _ = writeln!(out, "{}", line);
            }
        });
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        }
    }
}

/// 取前 N 行。
fn head_lines(content: &str, n: usize) -> Vec<&str> {
    content.lines().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_basic() {
        let lines = head_lines("a\nb\nc\n", 2);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn head_more_than_available() {
        let lines = head_lines("a\nb\n", 10);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn head_zero() {
        let lines = head_lines("a\nb\n", 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn head_empty_input() {
        let lines = head_lines("", 5);
        assert!(lines.is_empty());
    }

    #[test]
    fn head_no_trailing_newline() {
        let lines = head_lines("a\nb\nc", 2);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn head_single_line() {
        let lines = head_lines("only line\n", 1);
        assert_eq!(lines, vec!["only line"]);
    }
}
