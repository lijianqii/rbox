//! `grep` - 文本搜索。
use crate::applet::Applet;
use std::io::{Read, Write};
use std::process::ExitCode;

pub struct Grep;
pub static GREP: &Grep = &Grep;

impl Applet for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn help(&self) -> &'static str {
        "grep [-i] [-n] [-v] PATTERN [file] - search text"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let opts = parse_grep_args(args);
        match opts {
            GrepOpts::Error(msg) => {
                eprintln!("grep: {}", msg);
                ExitCode::from(2)
            }
            GrepOpts::Ok {
                pattern,
                ignore_case,
                show_line_num,
                invert,
                files,
            } => {
                let mut out = std::io::stdout().lock();
                let mut found = false;
                let multi = files.len() > 1;

                let mut search = |content: &str, fname: Option<&str>| {
                    for line in
                        grep_search(content, pattern, ignore_case, show_line_num, invert, fname)
                    {
                        let _ = writeln!(out, "{}", line);
                        found = true;
                    }
                };

                if files.is_empty() {
                    let mut buf = Vec::new();
                    if std::io::stdin().lock().read_to_end(&mut buf).is_ok() {
                        let content = String::from_utf8_lossy(&buf);
                        search(&content, None);
                    }
                } else {
                    for f in &files {
                        match std::fs::read(f) {
                            Ok(bytes) => {
                                let content = String::from_utf8_lossy(&bytes);
                                let fname = if multi { Some(f.as_str()) } else { None };
                                search(&content, fname);
                            }
                            Err(e) => eprintln!("grep: {}: {}", f, e),
                        }
                    }
                }

                if found {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// grep 选项解析结果。
enum GrepOpts<'a> {
    Error(&'static str),
    Ok {
        pattern: &'a str,
        ignore_case: bool,
        show_line_num: bool,
        invert: bool,
        files: Vec<&'a String>,
    },
}

fn parse_grep_args<'a>(args: &'a [String]) -> GrepOpts<'a> {
    let mut ignore_case = false;
    let mut show_line_num = false;
    let mut invert = false;
    let mut positional: Vec<&String> = Vec::new();
    let mut end_of_opts = false;

    for a in args {
        if end_of_opts {
            positional.push(a);
            continue;
        }
        match a.as_str() {
            "--" => end_of_opts = true,
            "-" => positional.push(a), // "-" 表示 stdin，作为位置参数处理
            s if s.starts_with('-') && s.len() > 1 => {
                // 逐字符解析组合标志（如 -inv）
                for c in s[1..].chars() {
                    match c {
                        'i' => ignore_case = true,
                        'n' => show_line_num = true,
                        'v' => invert = true,
                        _ => return GrepOpts::Error("unknown option"),
                    }
                }
            }
            _ => positional.push(a),
        }
    }

    if positional.is_empty() {
        return GrepOpts::Error("missing PATTERN");
    }

    let pattern = positional[0];
    let files = positional[1..].to_vec();

    GrepOpts::Ok {
        pattern,
        ignore_case,
        show_line_num,
        invert,
        files,
    }
}

/// 在内容中搜索匹配行，返回格式化后的行列表。
fn grep_search(
    content: &str,
    pattern: &str,
    ignore_case: bool,
    show_line_num: bool,
    invert: bool,
    fname: Option<&str>,
) -> Vec<String> {
    let pat = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let mut result = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let target = if ignore_case {
            line.to_lowercase()
        } else {
            line.to_string()
        };
        let matches = target.contains(&pat);
        if matches != invert {
            let prefix = match (show_line_num, fname) {
                (true, Some(f)) => format!("{}:{}: ", f, i + 1),
                (true, None) => format!("{}: ", i + 1),
                (false, Some(f)) => format!("{}: ", f),
                (false, None) => String::new(),
            };
            result.push(format!("{}{}", prefix, line));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_match() {
        let lines = grep_search("hello\nworld\n", "hello", false, false, false, None);
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn no_match() {
        let lines = grep_search("hello\nworld\n", "xyz", false, false, false, None);
        assert!(lines.is_empty());
    }

    #[test]
    fn ignore_case() {
        let lines = grep_search("Hello World\n", "hello", true, false, false, None);
        assert_eq!(lines, vec!["Hello World"]);
    }

    #[test]
    fn invert_match() {
        let lines = grep_search("hello\nworld\n", "hello", false, false, true, None);
        assert_eq!(lines, vec!["world"]);
    }

    #[test]
    fn line_number() {
        let lines = grep_search("a\nb\nc\n", "b", false, true, false, None);
        assert_eq!(lines, vec!["2: b"]);
    }

    #[test]
    fn filename_prefix() {
        let lines = grep_search("hello\n", "hello", false, false, false, Some("test.txt"));
        assert_eq!(lines, vec!["test.txt: hello"]);
    }

    #[test]
    fn line_number_and_filename() {
        let lines = grep_search("a\nb\n", "b", false, true, false, Some("f.txt"));
        assert_eq!(lines, vec!["f.txt:2: b"]);
    }

    #[test]
    fn multiple_matches() {
        let lines = grep_search("ab\nac\nbd\n", "a", false, false, false, None);
        assert_eq!(lines, vec!["ab", "ac"]);
    }

    #[test]
    fn parse_basic() {
        let args = vec!["hello".to_string()];
        let opts = parse_grep_args(&args);
        match opts {
            GrepOpts::Ok { pattern, .. } => assert_eq!(pattern, "hello"),
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn parse_missing_pattern() {
        let args: Vec<String> = vec![];
        let opts = parse_grep_args(&args);
        assert!(matches!(opts, GrepOpts::Error(_)));
    }

    #[test]
    fn parse_combined_flags() {
        let args = vec!["-in".to_string(), "hello".to_string()];
        let opts = parse_grep_args(&args);
        match opts {
            GrepOpts::Ok {
                ignore_case: true,
                show_line_num: true,
                ..
            } => {}
            _ => panic!("expected Ok with flags"),
        }
    }

    #[test]
    fn parse_triple_flags() {
        let args = vec!["-inv".to_string(), "hello".to_string()];
        let opts = parse_grep_args(&args);
        match opts {
            GrepOpts::Ok {
                ignore_case: true,
                show_line_num: true,
                invert: true,
                ..
            } => {}
            _ => panic!("expected Ok with all three flags"),
        }
    }

    #[test]
    fn parse_end_of_opts() {
        // grep -- -pattern should treat -pattern as positional (pattern to search)
        let args = vec!["--".to_string(), "-pattern".to_string()];
        let opts = parse_grep_args(&args);
        match opts {
            GrepOpts::Ok { pattern, files, .. } => {
                assert_eq!(pattern, "-pattern");
                assert!(files.is_empty());
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn parse_unknown_option() {
        let args = vec!["-x".to_string(), "hello".to_string()];
        let opts = parse_grep_args(&args);
        assert!(matches!(opts, GrepOpts::Error(_)));
    }
}
