//! `wc` - 统计行数/单词数/字节数。
use crate::applet::Applet;
use std::io::{Read, Write};
use std::process::ExitCode;

pub struct Wc;
pub static WC: &Wc = &Wc;

/// 计数选项 + 统计结果。
struct WcCounts {
    lines: bool,
    words: bool,
    bytes: bool,
}

impl WcCounts {
    fn new(lines: bool, words: bool, bytes: bool) -> Self {
        Self {
            lines,
            words,
            bytes,
        }
    }

    /// 格式化输出一行计数。
    fn print(&self, l: usize, w: usize, b: usize, name: &str, out: &mut impl Write) {
        let mut parts: Vec<String> = Vec::new();
        if self.lines {
            parts.push(format!("{:>7}", l));
        }
        if self.words {
            parts.push(format!("{:>7}", w));
        }
        if self.bytes {
            parts.push(format!("{:>7}", b));
        }
        if name.is_empty() {
            let _ = writeln!(out, "{}", parts.join(" "));
        } else {
            let _ = writeln!(out, "{} {}", parts.join(" "), name);
        }
    }
}

impl Applet for Wc {
    fn name(&self) -> &'static str {
        "wc"
    }
    fn help(&self) -> &'static str {
        "wc [-l] [-w] [-c] [file] - count lines/words/bytes"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        // 启用式解析：指定任一选项则只统计指定的；未指定默认全部
        let mut count_lines = false;
        let mut count_words = false;
        let mut count_bytes = false;
        let mut any_specified = false;
        let mut files: Vec<&String> = Vec::new();

        for a in args {
            match a.as_str() {
                "-l" => {
                    count_lines = true;
                    any_specified = true;
                }
                "-w" => {
                    count_words = true;
                    any_specified = true;
                }
                "-c" => {
                    count_bytes = true;
                    any_specified = true;
                }
                "-lw" | "-wl" => {
                    count_lines = true;
                    count_words = true;
                    any_specified = true;
                }
                "-lc" | "-cl" => {
                    count_lines = true;
                    count_bytes = true;
                    any_specified = true;
                }
                "-wc" | "-cw" => {
                    count_words = true;
                    count_bytes = true;
                    any_specified = true;
                }
                "-lwc" | "-lcw" | "-wlc" | "-wcl" | "-clw" | "-cwl" => {
                    count_lines = true;
                    count_words = true;
                    count_bytes = true;
                    any_specified = true;
                }
                "-" => {}
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("wc: unknown option: {}", s);
                }
                _ => files.push(a),
            }
        }
        if !any_specified {
            count_lines = true;
            count_words = true;
            count_bytes = true;
        }

        let counts = WcCounts::new(count_lines, count_words, count_bytes);
        let mut out = std::io::stdout().lock();
        let mut total_l = 0usize;
        let mut total_w = 0usize;
        let mut total_b = 0usize;

        let process = |content: &str| -> (usize, usize, usize) {
            let lines = content.lines().count();
            let words = content.split_whitespace().count();
            let bytes = content.len();
            (lines, words, bytes)
        };

        if files.is_empty() {
            let mut buf = String::new();
            if std::io::stdin().lock().read_to_string(&mut buf).is_ok() {
                let (l, w, b) = process(&buf);
                counts.print(l, w, b, "", &mut out);
            }
        } else {
            for f in &files {
                match std::fs::read_to_string(f) {
                    Ok(content) => {
                        let (l, w, b) = process(&content);
                        total_l += l;
                        total_w += w;
                        total_b += b;
                        counts.print(l, w, b, f, &mut out);
                    }
                    Err(e) => eprintln!("wc: {}: {}", f, e),
                }
            }
            if files.len() > 1 {
                counts.print(total_l, total_w, total_b, "total", &mut out);
            }
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_basic() {
        let counts = WcCounts::new(true, true, true);
        // 2 lines, 4 words, 16 bytes
        let content = "hello world\nfoo bar\n";
        let lines = content.lines().count();
        let words = content.split_whitespace().count();
        let bytes = content.len();
        assert_eq!((lines, words, bytes), (2, 4, 20));
    }

    #[test]
    fn count_empty() {
        let content = "";
        assert_eq!(content.lines().count(), 0);
        assert_eq!(content.split_whitespace().count(), 0);
        assert_eq!(content.len(), 0);
    }

    #[test]
    fn count_single_line() {
        let content = "hello";
        assert_eq!(content.lines().count(), 1);
        assert_eq!(content.split_whitespace().count(), 1);
        assert_eq!(content.len(), 5);
    }
}
