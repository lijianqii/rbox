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
        let mut option_error = false;
        let mut files: Vec<&String> = Vec::new();

        for a in args {
            if a == "-" {
                continue;
            }
            if a.starts_with('-') && a.len() > 1 {
                // 逐字符解析选项
                for c in a[1..].chars() {
                    match c {
                        'l' => {
                            count_lines = true;
                            any_specified = true;
                        }
                        'w' => {
                            count_words = true;
                            any_specified = true;
                        }
                        'c' => {
                            count_bytes = true;
                            any_specified = true;
                        }
                        _ => {
                            eprintln!("wc: unknown option: -{}", c);
                            option_error = true;
                        }
                    }
                }
            } else {
                files.push(a);
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

        let mut had_error = option_error;

        if files.is_empty() {
            let mut buf = Vec::new();
            match std::io::stdin().lock().read_to_end(&mut buf) {
                Ok(_) => {
                    let (l, w, b) = count_bytes_data(&buf);
                    counts.print(l, w, b, "", &mut out);
                }
                Err(e) => {
                    eprintln!("wc: stdin: {}", e);
                    had_error = true;
                }
            }
        } else {
            for f in &files {
                match std::fs::read(f) {
                    Ok(content) => {
                        let (l, w, b) = count_bytes_data(&content);
                        total_l += l;
                        total_w += w;
                        total_b += b;
                        counts.print(l, w, b, f, &mut out);
                    }
                    Err(e) => {
                        eprintln!("wc: {}: {}", f, e);
                        had_error = true;
                    }
                }
            }
            if files.len() > 1 {
                counts.print(total_l, total_w, total_b, "total", &mut out);
            }
        }

        if had_error {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// 统计字节内容的行数/单词数/字节数。
/// 行数按 `\n` 字节计数（与 GNU wc 一致）；单词数通过 lossy UTF-8 文本切分。
fn count_bytes_data(content: &[u8]) -> (usize, usize, usize) {
    let lines = content.iter().filter(|&&b| b == b'\n').count();
    let text = String::from_utf8_lossy(content);
    let words = text.split_whitespace().count();
    let bytes = content.len();
    (lines, words, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_basic() {
        // 2 newlines, 4 words, 20 bytes
        let content = b"hello world\nfoo bar\n";
        assert_eq!(count_bytes_data(content), (2, 4, 20));
    }

    #[test]
    fn count_empty() {
        assert_eq!(count_bytes_data(b""), (0, 0, 0));
    }

    #[test]
    fn count_single_line() {
        // GNU wc -l 按换行符计数，"hello" 没有换行所以行数为 0
        assert_eq!(count_bytes_data(b"hello"), (0, 1, 5));
    }

    #[test]
    fn count_binary_bytes() {
        // 非法 UTF-8 不应导致统计失败，字节数按原始长度计算
        let content = b"\xff\xfe\n\x00abc";
        assert_eq!(count_bytes_data(content), (1, 2, 7));
    }

    #[test]
    fn unknown_option_returns_failure() {
        let code = WC.run(&["-x".to_string()]);
        assert_ne!(code, std::process::ExitCode::SUCCESS);
    }
}
