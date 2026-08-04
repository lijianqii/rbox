//! `echo` - 输出参数。
use crate::applet::Applet;
use std::io::Write;
use std::process::ExitCode;

pub struct Echo;
pub static ECHO: &Echo = &Echo;

impl Applet for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn help(&self) -> &'static str {
        "echo [-n] [args...] - print arguments"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (output, newline) = echo_format(args);
        print!("{}", output);
        if newline {
            println!();
        }
        match std::io::stdout().flush() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        }
    }
}

/// 将参数格式化为字符串，返回 (输出文本, 是否追加换行)。
fn echo_format(args: &[String]) -> (String, bool) {
    let mut args = args.iter();
    let mut newline = true;
    let mut result = String::new();

    if let Some(first) = args.next() {
        if first == "-n" {
            newline = false;
        } else {
            result.push_str(first);
        }
    }

    for a in args {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(a);
    }

    (result, newline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_text() {
        assert_eq!(echo_format(&["hello".into()]), ("hello".to_string(), true));
    }

    #[test]
    fn multiple_args() {
        assert_eq!(
            echo_format(&["hello".into(), "world".into()]),
            ("hello world".to_string(), true)
        );
    }

    #[test]
    fn no_newline() {
        assert_eq!(
            echo_format(&["-n".into(), "hello".into()]),
            ("hello".to_string(), false)
        );
    }

    #[test]
    fn no_newline_multiple() {
        assert_eq!(
            echo_format(&["-n".into(), "a".into(), "b".into(), "c".into()]),
            ("a b c".to_string(), false)
        );
    }

    #[test]
    fn empty_args() {
        let (output, newline) = echo_format(&[]);
        assert_eq!(output, "");
        assert!(newline);
    }

    #[test]
    fn dash_n_only() {
        let (output, newline) = echo_format(&["-n".into()]);
        assert_eq!(output, "");
        assert!(!newline);
    }

    #[test]
    fn second_dash_n_is_text() {
        // Only first -n is treated as flag
        assert_eq!(
            echo_format(&["-n".into(), "-n".into()]),
            ("-n".to_string(), false)
        );
    }
}
