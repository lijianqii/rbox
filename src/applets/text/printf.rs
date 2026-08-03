//! `printf` - 格式化输出。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Printf;
pub static PRINTF: &Printf = &Printf;

impl Applet for Printf {
    fn name(&self) -> &'static str {
        "printf"
    }
    fn help(&self) -> &'static str {
        "printf FORMAT [args] - formatted output"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("printf: missing FORMAT");
            return ExitCode::from(1);
        }
        let format = &args[0];
        let format_args = &args[1..];
        print!("{}", printf_format(format, format_args));
        ExitCode::SUCCESS
    }
}

/// 格式化输出：处理 `%s` `%d` `%x` `%c` `%%` 和 `\n` `\t` `\\` 等转义。
fn printf_format(format: &str, args: &[String]) -> String {
    let mut result = String::new();
    let mut arg_idx = 0;
    let mut chars = format.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some('0') => result.push('\0'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => break,
            },
            '%' => match chars.next() {
                Some('s') => {
                    if arg_idx < args.len() {
                        result.push_str(&args[arg_idx]);
                        arg_idx += 1;
                    }
                }
                Some('d') => {
                    if arg_idx < args.len() {
                        match args[arg_idx].parse::<i64>() {
                            Ok(n) => result.push_str(&n.to_string()),
                            Err(_) => result.push('0'),
                        }
                        arg_idx += 1;
                    }
                }
                Some('x') => {
                    if arg_idx < args.len() {
                        match args[arg_idx].parse::<u64>() {
                            Ok(n) => result.push_str(&format!("{:x}", n)),
                            Err(_) => result.push('0'),
                        }
                        arg_idx += 1;
                    }
                }
                Some('c') => {
                    if arg_idx < args.len() {
                        if let Some(ch) = args[arg_idx].chars().next() {
                            result.push(ch);
                        }
                        arg_idx += 1;
                    }
                }
                Some('%') => result.push('%'),
                Some(other) => {
                    result.push('%');
                    result.push(other);
                }
                None => {
                    result.push('%');
                    break;
                }
            },
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_s() {
        assert_eq!(printf_format("hello %s", &["world".into()]), "hello world");
    }

    #[test]
    fn percent_d() {
        assert_eq!(printf_format("num=%d", &["42".into()]), "num=42");
    }

    #[test]
    fn percent_d_invalid() {
        assert_eq!(printf_format("num=%d", &["abc".into()]), "num=0");
    }

    #[test]
    fn percent_x() {
        assert_eq!(printf_format("hex=%x", &["255".into()]), "hex=ff");
    }

    #[test]
    fn percent_c() {
        assert_eq!(printf_format("char=%c", &["abc".into()]), "char=a");
    }

    #[test]
    fn percent_percent() {
        assert_eq!(printf_format("100%%", &[]), "100%");
    }

    #[test]
    fn escape_newline() {
        assert_eq!(printf_format("line1\\nline2", &[]), "line1\nline2");
    }

    #[test]
    fn escape_tab() {
        assert_eq!(printf_format("a\\tb", &[]), "a\tb");
    }

    #[test]
    fn escape_backslash() {
        assert_eq!(printf_format("a\\\\b", &[]), "a\\b");
    }

    #[test]
    fn multiple_args() {
        assert_eq!(printf_format("%s=%d", &["x".into(), "10".into()]), "x=10");
    }

    #[test]
    fn no_args() {
        assert_eq!(printf_format("hello", &[]), "hello");
    }

    #[test]
    fn missing_arg_s() {
        // %s with no arg -> empty string
        assert_eq!(printf_format("val=%s", &[]), "val=");
    }
}
