//! `uniq` - 去除相邻重复行。
//!
//! 用法：uniq [-c] [-d] [-u] [file]
//! - `-c` 行首显示重复次数；`-d` 只显示重复行；`-u` 只显示不重复行。

use crate::applet::Applet;
use crate::applets::text::util::read_input_lines;
use std::process::ExitCode;

pub struct Uniq;
pub static UNIQ: &Uniq = &Uniq;

/// uniq 选项。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct UniqOpts {
    pub(crate) count: bool,
    pub(crate) only_dup: bool,
    pub(crate) only_uniq: bool,
}

/// 对行做相邻去重，返回输出行。
pub(crate) fn uniq_lines(lines: &[String], opts: UniqOpts) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines[j] == lines[i] {
            j += 1;
        }
        let count = j - i;
        let dup = count > 1;
        let show = if opts.only_dup {
            dup
        } else if opts.only_uniq {
            !dup
        } else {
            true
        };
        if show {
            if opts.count {
                out.push(format!("{:>7} {}", count, lines[i]));
            } else {
                out.push(lines[i].clone());
            }
        }
        i = j;
    }
    out
}

/// 解析参数，返回 (选项, 文件列表)。
pub(crate) fn parse_args(args: &[String]) -> Result<(UniqOpts, Vec<String>), String> {
    let mut opts = UniqOpts::default();
    let mut files = Vec::new();
    let mut end_of_options = false;
    for a in args {
        if !end_of_options && a == "--" {
            end_of_options = true;
            continue;
        }
        if !end_of_options && a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'c' => opts.count = true,
                    'd' => opts.only_dup = true,
                    'u' => opts.only_uniq = true,
                    other => return Err(format!("invalid option -- '{}'", other)),
                }
            }
        } else {
            files.push(a.clone());
        }
    }
    Ok((opts, files))
}

impl Applet for Uniq {
    fn name(&self) -> &'static str {
        "uniq"
    }
    fn help(&self) -> &'static str {
        "uniq [-cdu] [file] - filter adjacent duplicate lines"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (opts, files) = match parse_args(args) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("uniq: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let (lines, ok) = read_input_lines(&files, "uniq");
        for line in uniq_lines(&lines, opts) {
            println!("{}", line);
        }
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn name_and_help() {
        assert_eq!(UNIQ.name(), "uniq");
        assert!(UNIQ.help().contains("duplicate"));
    }

    #[test]
    fn removes_adjacent_duplicates() {
        let out = uniq_lines(&lines(&["a", "a", "b", "a"]), UniqOpts::default());
        assert_eq!(out, lines(&["a", "b", "a"]));
    }

    #[test]
    fn count_mode() {
        let opts = UniqOpts {
            count: true,
            ..Default::default()
        };
        let out = uniq_lines(&lines(&["a", "a", "b"]), opts);
        assert_eq!(out, vec!["      2 a".to_string(), "      1 b".to_string()]);
    }

    #[test]
    fn only_dup_and_only_uniq() {
        let dup = UniqOpts {
            only_dup: true,
            ..Default::default()
        };
        assert_eq!(
            uniq_lines(&lines(&["a", "a", "b"]), dup),
            vec!["a".to_string()]
        );
        let uniq = UniqOpts {
            only_uniq: true,
            ..Default::default()
        };
        assert_eq!(
            uniq_lines(&lines(&["a", "a", "b"]), uniq),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn parse_flags() {
        let (opts, files) = parse_args(&["-c".to_string(), "f".to_string()]).unwrap();
        assert!(opts.count);
        assert_eq!(files, vec!["f"]);
        assert!(parse_args(&["-x".to_string()]).is_err());
    }
}
