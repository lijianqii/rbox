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
            for line in tail_lines(content, n) {
                let _ = writeln!(out, "{}", line);
            }
        });
        ExitCode::SUCCESS
    }
}

/// 取后 N 行。
fn tail_lines(content: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let start = if lines.len() > n { lines.len() - n } else { 0 };
    lines[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_basic() {
        let lines = tail_lines("a\nb\nc\n", 2);
        assert_eq!(lines, vec!["b", "c"]);
    }

    #[test]
    fn tail_more_than_available() {
        let lines = tail_lines("a\nb\n", 10);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn tail_zero() {
        let lines = tail_lines("a\nb\n", 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn tail_empty_input() {
        let lines = tail_lines("", 5);
        assert!(lines.is_empty());
    }

    #[test]
    fn tail_single_line() {
        let lines = tail_lines("only line\n", 1);
        assert_eq!(lines, vec!["only line"]);
    }

    #[test]
    fn tail_exact_count() {
        let lines = tail_lines("a\nb\nc\n", 3);
        assert_eq!(lines, vec!["a", "b", "c"]);
    }
}
