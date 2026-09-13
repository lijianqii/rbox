//! `sort` - 按行排序。
//!
//! 用法：sort [-n] [-r] [-u] [-f] [file...]
//! - `-n` 数值排序；`-r` 逆序；`-u` 去重；`-f` 忽略大小写。
//!
//! 无文件或 `-` 时读 stdin。

use crate::applet::Applet;
use crate::applets::text::util::read_input_lines;
use std::process::ExitCode;

pub struct Sort;
pub static SORT: &Sort = &Sort;

/// 排序选项。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct SortOpts {
    pub(crate) numeric: bool,
    pub(crate) reverse: bool,
    pub(crate) unique: bool,
    pub(crate) fold_case: bool,
}

/// 数值键：前缀可解析为 f64 时用其值，否则按 0 排前。
fn numeric_key(line: &str) -> f64 {
    line.split_whitespace()
        .next()
        .and_then(|t| t.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// 排序（稳定排序；返回排序后的行）。
pub(crate) fn sort_lines(mut lines: Vec<String>, opts: SortOpts) -> Vec<String> {
    lines.sort_by(|a, b| {
        let ord = if opts.numeric {
            numeric_key(a)
                .partial_cmp(&numeric_key(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        } else if opts.fold_case {
            a.to_lowercase().cmp(&b.to_lowercase())
        } else {
            a.cmp(b)
        };
        if opts.reverse { ord.reverse() } else { ord }
    });
    if opts.unique {
        lines.dedup();
    }
    lines
}

/// 解析参数，返回 (选项, 文件列表)。
pub(crate) fn parse_args(args: &[String]) -> Result<(SortOpts, Vec<String>), String> {
    let mut opts = SortOpts::default();
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
                    'n' => opts.numeric = true,
                    'r' => opts.reverse = true,
                    'u' => opts.unique = true,
                    'f' => opts.fold_case = true,
                    other => return Err(format!("invalid option -- '{}'", other)),
                }
            }
        } else {
            files.push(a.clone());
        }
    }
    Ok((opts, files))
}

impl Applet for Sort {
    fn name(&self) -> &'static str {
        "sort"
    }
    fn help(&self) -> &'static str {
        "sort [-nruf] [file...] - sort lines of text"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (opts, files) = match parse_args(args) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("sort: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let (collected, ok) = read_input_lines(&files, "sort");
        for line in sort_lines(collected, opts) {
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
        assert_eq!(SORT.name(), "sort");
        assert!(SORT.help().contains("sort"));
    }

    #[test]
    fn lexical_sort() {
        let out = sort_lines(lines(&["b", "a", "c"]), SortOpts::default());
        assert_eq!(out, lines(&["a", "b", "c"]));
    }

    #[test]
    fn numeric_sort() {
        let opts = SortOpts {
            numeric: true,
            ..Default::default()
        };
        assert_eq!(
            sort_lines(lines(&["10", "2", "1"]), opts),
            lines(&["1", "2", "10"])
        );
    }

    #[test]
    fn reverse_and_unique() {
        let opts = SortOpts {
            reverse: true,
            ..Default::default()
        };
        assert_eq!(
            sort_lines(lines(&["a", "c", "b"]), opts),
            lines(&["c", "b", "a"])
        );
        let opts = SortOpts {
            unique: true,
            ..Default::default()
        };
        assert_eq!(
            sort_lines(lines(&["a", "a", "b"]), opts),
            lines(&["a", "b"])
        );
    }

    #[test]
    fn fold_case() {
        let opts = SortOpts {
            fold_case: true,
            ..Default::default()
        };
        assert_eq!(sort_lines(lines(&["b", "A"]), opts), lines(&["A", "b"]));
    }

    #[test]
    fn parse_options_and_files() {
        let (opts, files) = parse_args(&["-nr".to_string(), "f.txt".to_string()]).unwrap();
        assert!(opts.numeric && opts.reverse);
        assert_eq!(files, vec!["f.txt"]);
        assert!(parse_args(&["-x".to_string()]).is_err());
    }
}
