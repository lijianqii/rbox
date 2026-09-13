//! `tr` - 转换/删除/压缩字符。
//!
//! 用法：tr [-d] [-s] SET1 [SET2]
//! - 默认：SET1 中字符映射到 SET2 对应字符（SET2 较短时重复最后一个字符）；
//! - `-d`：删除 SET1 中字符；`-s`：压缩 SET1（或 SET2）中的连续重复字符；
//! - 支持 `a-z` 范围与 `\n` `\t` `\\` `\NNN` 转义。仅从 stdin 读取。

use crate::applet::Applet;
use std::io::Read;
use std::process::ExitCode;

pub struct Tr;
pub static TR: &Tr = &Tr;

/// 展开字符集（支持 `a-z` 范围与转义）。
pub(crate) fn expand_set(spec: &str) -> Result<Vec<char>, String> {
    let chars: Vec<char> = spec.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            let mapped = match next {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                'a' => '\x07',
                'b' => '\x08',
                'f' => '\x0c',
                'v' => '\x0b',
                '0' => '\0',
                '1'..='7' => {
                    // 最多 3 位八进制
                    let mut val = next.to_digit(8).unwrap();
                    let mut j = i + 2;
                    while j < chars.len() && j < i + 4 {
                        if let Some(d) = chars[j].to_digit(8) {
                            val = val * 8 + d;
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    i = j - 2; // 循环末尾 i += 2 后指向下一位
                    char::from_u32(val).unwrap_or('\0')
                }
                other => other,
            };
            out.push(mapped);
            i += 2;
            continue;
        }
        // 范围 a-z（两端均为普通字符）
        if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != '\\' {
            let (a, b) = (c, chars[i + 2]);
            if (a as u32) <= (b as u32) {
                for v in (a as u32)..=(b as u32) {
                    if let Some(ch) = char::from_u32(v) {
                        out.push(ch);
                    }
                }
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// 应用 tr 操作。
pub(crate) fn translate(
    input: &str,
    set1: &[char],
    set2: &[char],
    delete: bool,
    squeeze: bool,
) -> String {
    let mut out = String::new();
    let mut last: Option<char> = None;
    for c in input.chars() {
        if delete && set1.contains(&c) {
            continue;
        }
        let mapped = if !delete && !set2.is_empty() {
            match set1.iter().position(|&x| x == c) {
                Some(idx) => set2[idx.min(set2.len() - 1)],
                None => c,
            }
        } else {
            c
        };
        if squeeze {
            let squeeze_set = if set2.is_empty() { set1 } else { set2 };
            if squeeze_set.contains(&mapped) && last == Some(mapped) {
                continue;
            }
        }
        out.push(mapped);
        last = Some(mapped);
    }
    out
}

impl Applet for Tr {
    fn name(&self) -> &'static str {
        "tr"
    }
    fn help(&self) -> &'static str {
        "tr [-d] [-s] SET1 [SET2] - translate, delete or squeeze characters"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut delete = false;
        let mut squeeze = false;
        let mut sets: Vec<&str> = Vec::new();
        for a in args {
            match a.as_str() {
                "-d" | "--delete" => delete = true,
                "-s" | "--squeeze-repeats" => squeeze = true,
                "-ds" | "-sd" => {
                    delete = true;
                    squeeze = true;
                }
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("tr: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                s => sets.push(s),
            }
        }
        if sets.is_empty() {
            eprintln!("tr: usage: tr [-d] [-s] SET1 [SET2]");
            return ExitCode::FAILURE;
        }
        let set1 = match expand_set(sets[0]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("tr: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let set2 = if sets.len() > 1 {
            match expand_set(sets[1]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("tr: {}", e);
                    return ExitCode::FAILURE;
                }
            }
        } else {
            Vec::new()
        };
        if !delete && set2.is_empty() && !squeeze {
            eprintln!("tr: missing operand");
            return ExitCode::FAILURE;
        }

        let mut buf = Vec::new();
        if std::io::stdin().read_to_end(&mut buf).is_err() {
            eprintln!("tr: read error");
            return ExitCode::FAILURE;
        }
        let input = String::from_utf8_lossy(&buf);
        print!("{}", translate(&input, &set1, &set2, delete, squeeze));
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(TR.name(), "tr");
        assert!(TR.help().contains("translate"));
    }

    #[test]
    fn expand_ranges_and_escapes() {
        assert_eq!(expand_set("a-c").unwrap(), vec!['a', 'b', 'c']);
        assert_eq!(expand_set("\\n").unwrap(), vec!['\n']);
        assert_eq!(expand_set("\\t").unwrap(), vec!['\t']);
        assert_eq!(expand_set("x\\\\").unwrap(), vec!['x', '\\']);
        assert_eq!(expand_set("\\101").unwrap(), vec!['A']);
    }

    #[test]
    fn translate_case() {
        let s1 = expand_set("a-z").unwrap();
        let s2 = expand_set("A-Z").unwrap();
        assert_eq!(translate("hello", &s1, &s2, false, false), "HELLO");
    }

    #[test]
    fn translate_repeats_last_char() {
        assert_eq!(
            translate("abc", &['a', 'b', 'c'], &['x'], false, false),
            "xxx"
        );
    }

    #[test]
    fn delete_and_squeeze() {
        assert_eq!(
            translate(
                "a1b2c3",
                &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'],
                &[],
                true,
                false
            ),
            "abc"
        );
        assert_eq!(
            translate("aabbbcc", &['a', 'b', 'c'], &[], false, true),
            "abc"
        );
    }

    #[test]
    fn squeeze_after_translate() {
        // tr -s 'a' 'b'：a->b 后压缩 b
        assert_eq!(translate("aaab", &['a'], &['b'], false, true), "b");
    }
}
