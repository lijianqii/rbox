//! `cut` - 按分隔符取字段 / 按字符位置截取。
//!
//! 用法：cut -d DELIM -f LIST [-s] [file...]
//!       cut -c LIST [file...]
//! LIST 形如 `1,3-5`（1-based，含端点）。

use crate::applet::Applet;
use crate::applets::text::util::read_input_lines;
use std::process::ExitCode;

pub struct Cut;
pub static CUT: &Cut = &Cut;

/// 解析位置列表（如 `1,3-5`）为 1-based 索引集合。
pub(crate) fn parse_list(spec: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("invalid field list: '{}'", spec));
        }
        if let Some((a, b)) = part.split_once('-') {
            let a: usize = a
                .parse()
                .map_err(|_| format!("invalid range: '{}'", part))?;
            let b: usize = if b.is_empty() {
                usize::MAX // `3-` 到行尾
            } else {
                b.parse()
                    .map_err(|_| format!("invalid range: '{}'", part))?
            };
            if a == 0 || b < a {
                return Err(format!("invalid range: '{}'", part));
            }
            // 限制展开上限，避免 `1-` 展开成巨大列表
            let end = b.min(a + 65535);
            out.extend(a..=end);
        } else {
            let n: usize = part
                .parse()
                .map_err(|_| format!("invalid field: '{}'", part))?;
            if n == 0 {
                return Err(format!("invalid field: '{}'", part));
            }
            out.push(n);
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// 按分隔符取字段；`only_delimited=false` 时无分隔符的行原样输出（cut 默认）。
pub(crate) fn cut_fields(
    line: &str,
    delim: char,
    fields: &[usize],
    only_delimited: bool,
) -> Option<String> {
    let parts: Vec<&str> = line.split(delim).collect();
    if parts.len() == 1 && !line.contains(delim) {
        return if only_delimited {
            None
        } else {
            Some(line.to_string())
        };
    }
    let selected: Vec<&str> = fields
        .iter()
        .filter_map(|&n| parts.get(n - 1).copied())
        .collect();
    Some(selected.join(&delim.to_string()))
}

/// 按字符位置截取（1-based）。
pub(crate) fn cut_chars(line: &str, fields: &[usize]) -> String {
    let chars: Vec<char> = line.chars().collect();
    fields
        .iter()
        .filter_map(|&n| chars.get(n - 1).copied())
        .collect()
}

/// cut 解析结果。
pub(crate) struct CutArgs {
    pub(crate) delim: Option<char>,
    pub(crate) fields: Vec<usize>,
    pub(crate) only_delimited: bool,
    pub(crate) char_mode: bool,
    pub(crate) files: Vec<String>,
}

/// 解析参数。
pub(crate) fn parse_args(args: &[String]) -> Result<CutArgs, String> {
    let mut delim: Option<char> = None;
    let mut fields: Vec<usize> = Vec::new();
    let mut only_delimited = false;
    let mut char_mode = false;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-d" => {
                i += 1;
                let Some(d) = args.get(i) else {
                    return Err("option -d requires an argument".to_string());
                };
                let mut it = d.chars();
                let c = it.next().ok_or("empty delimiter")?;
                if it.next().is_some() {
                    return Err("delimiter must be a single character".to_string());
                }
                delim = Some(c);
            }
            "-f" => {
                i += 1;
                let Some(f) = args.get(i) else {
                    return Err("option -f requires an argument".to_string());
                };
                fields = parse_list(f)?;
            }
            "-c" => {
                i += 1;
                let Some(f) = args.get(i) else {
                    return Err("option -c requires an argument".to_string());
                };
                fields = parse_list(f)?;
                char_mode = true;
            }
            "-s" => only_delimited = true,
            a if a.starts_with('-') && a.len() > 1 => {
                return Err(format!("invalid option -- '{}'", &a[1..]));
            }
            _ => files.push(args[i].clone()),
        }
        i += 1;
    }
    if fields.is_empty() {
        return Err("you must specify a list of fields".to_string());
    }
    if !char_mode && delim.is_none() {
        delim = Some('\t'); // cut 默认分隔符为 TAB
    }
    Ok(CutArgs {
        delim,
        fields,
        only_delimited,
        char_mode,
        files,
    })
}

impl Applet for Cut {
    fn name(&self) -> &'static str {
        "cut"
    }
    fn help(&self) -> &'static str {
        "cut -d DELIM -f LIST [-s] [file...] | cut -c LIST [file...]"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let parsed = match parse_args(args) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("cut: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let (lines, ok) = read_input_lines(&parsed.files, "cut");
        for line in lines {
            if parsed.char_mode {
                println!("{}", cut_chars(&line, &parsed.fields));
            } else if let Some(out) = cut_fields(
                &line,
                parsed.delim.unwrap_or('\t'),
                &parsed.fields,
                parsed.only_delimited,
            ) {
                println!("{}", out);
            }
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

    #[test]
    fn name_and_help() {
        assert_eq!(CUT.name(), "cut");
        assert!(CUT.help().contains("cut"));
    }

    #[test]
    fn list_parsing() {
        assert_eq!(parse_list("1,3").unwrap(), vec![1, 3]);
        assert_eq!(parse_list("2-4").unwrap(), vec![2, 3, 4]);
        assert!(parse_list("0").is_err());
        assert!(parse_list("4-2").is_err());
        assert!(parse_list("").is_err());
    }

    #[test]
    fn fields_with_delimiter() {
        assert_eq!(cut_fields("a:b:c", ':', &[1, 3], false).unwrap(), "a:c");
        assert_eq!(cut_fields("a:b:c", ':', &[2], false).unwrap(), "b");
        // 无分隔符：默认原样输出，-s 时跳过
        assert_eq!(cut_fields("plain", ':', &[1], false).unwrap(), "plain");
        assert_eq!(cut_fields("plain", ':', &[1], true), None);
    }

    #[test]
    fn chars_selection() {
        assert_eq!(cut_chars("abcdef", &[1, 3]), "ac");
        assert_eq!(cut_chars("中文", &[1, 2]), "中文");
        assert_eq!(cut_chars("ab", &[5]), "");
    }

    #[test]
    fn parse_defaults_to_tab() {
        let parsed = parse_args(&["-f".to_string(), "1".to_string()]).unwrap();
        assert_eq!(parsed.delim, Some('\t'));
        assert_eq!(parsed.fields, vec![1]);
        assert!(!parsed.char_mode);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_args(&["-f".to_string()]).is_err());
        assert!(parse_args(&["-x".to_string()]).is_err());
        assert!(parse_args(&[]).is_err());
    }
}
