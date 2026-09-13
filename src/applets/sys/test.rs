//! `test` / `[` - 条件表达式求值（shell 条件判断基础）。
//!
//! 用法：test EXPR
//!       [ EXPR ]
//!
//! 支持：
//! - 文件测试：`-e -f -d -r -w -x -s -L -h -b -c -p -S -t`
//! - 字符串：`-n -z`、`=` `!=`、单参数非空
//! - 数值：`-eq -ne -lt -le -gt -ge`
//! - 文件比较：`-nt -ot -ef`
//! - 逻辑：`!`、`-a`、`-o`、括号 `( )`
//!
//! 退出码：0 = 真，1 = 假，2 = 表达式错误。

use crate::applet::Applet;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;

pub struct Test;
pub static TEST: &Test = &Test;

pub struct Bracket;
pub static BRACKET: &Bracket = &Bracket;

/// 文件测试元数据（统一用 symlink_metadata，-L 不跟随，其余按需跟随）。
fn file_test(op: &str, path: &str) -> Option<bool> {
    let follow = fs::metadata(path);
    let link = fs::symlink_metadata(path);
    let exists_follow = follow.is_ok();
    Some(match op {
        "-e" => exists_follow,
        "-f" => follow.as_ref().map(|m| m.is_file()).unwrap_or(false),
        "-d" => follow.as_ref().map(|m| m.is_dir()).unwrap_or(false),
        "-s" => follow.as_ref().map(|m| m.len() > 0).unwrap_or(false),
        "-L" | "-h" => link
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false),
        "-b" => follow
            .as_ref()
            .map(|m| m.file_type().is_block_device())
            .unwrap_or(false),
        "-c" => follow
            .as_ref()
            .map(|m| m.file_type().is_char_device())
            .unwrap_or(false),
        "-p" => follow
            .as_ref()
            .map(|m| (m.mode() & libc::S_IFMT) == libc::S_IFIFO)
            .unwrap_or(false),
        "-S" => follow
            .as_ref()
            .map(|m| (m.mode() & libc::S_IFMT) == libc::S_IFSOCK)
            .unwrap_or(false),
        "-r" => unsafe { libc::access(cstr(path).as_ptr(), libc::R_OK) == 0 },
        "-w" => unsafe { libc::access(cstr(path).as_ptr(), libc::W_OK) == 0 },
        "-x" => unsafe { libc::access(cstr(path).as_ptr(), libc::X_OK) == 0 },
        _ => return None,
    })
}

fn cstr(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_default()
}

/// `-t FD`：fd 是否连接终端。
fn is_tty(fd: &str) -> Option<bool> {
    let n: i32 = fd.parse().ok()?;
    Some(unsafe { libc::isatty(n) } == 1)
}

/// 解析数值；非法返回 None。
fn parse_int(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

/// 比较文件 mtime（`-nt`：a 比 b 新；`-ot`：a 比 b 旧；文件不存在为假）。
fn mtime_compare(a: &str, b: &str, newer: bool) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    if newer {
        ma.modified().ok() > mb.modified().ok()
    } else {
        ma.modified().ok() < mb.modified().ok()
    }
}

/// `-ef`：同一设备与 inode。
fn same_file(a: &str, b: &str) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
        _ => false,
    }
}

/// 二元运算求值；不是二元运算返回 None。
fn binary(op: &str, lhs: &str, rhs: &str) -> Option<Result<bool, String>> {
    let r = match op {
        "=" | "==" => lhs == rhs,
        "!=" => lhs != rhs,
        "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
            let (Some(a), Some(b)) = (parse_int(lhs), parse_int(rhs)) else {
                return Some(Err(format!("integer expression expected: {} {}", lhs, op)));
            };
            match op {
                "-eq" => a == b,
                "-ne" => a != b,
                "-lt" => a < b,
                "-le" => a <= b,
                "-gt" => a > b,
                _ => a >= b,
            }
        }
        "-nt" => mtime_compare(lhs, rhs, true),
        "-ot" => mtime_compare(lhs, rhs, false),
        "-ef" => same_file(lhs, rhs),
        _ => return None,
    };
    Some(Ok(r))
}

/// 一元运算求值；不是一元运算返回 None。
fn unary(op: &str, arg: &str) -> Option<Result<bool, String>> {
    if op == "-t" {
        return Some(match is_tty(arg) {
            Some(v) => Ok(v),
            None => Err(format!("invalid fd: {}", arg)),
        });
    }
    if op == "-n" {
        return Some(Ok(!arg.is_empty()));
    }
    if op == "-z" {
        return Some(Ok(arg.is_empty()));
    }
    file_test(op, arg).map(Ok)
}

/// 是否为一元运算符。
fn is_unary_op(s: &str) -> bool {
    matches!(
        s,
        "-e" | "-f"
            | "-d"
            | "-r"
            | "-w"
            | "-x"
            | "-s"
            | "-L"
            | "-h"
            | "-b"
            | "-c"
            | "-p"
            | "-S"
            | "-t"
            | "-n"
            | "-z"
    )
}

/// 是否为二元运算符。
fn is_binary_op(s: &str) -> bool {
    matches!(
        s,
        "=" | "==" | "!=" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "-nt" | "-ot" | "-ef"
    )
}

/// 表达式解析器（递归下降）。
struct Parser<'a> {
    args: &'a [String],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.args.get(self.pos).map(String::as_str)
    }

    fn next(&mut self) -> Option<&'a str> {
        let v = self.peek();
        if v.is_some() {
            self.pos += 1;
        }
        v
    }

    /// or := and ( '-o' and )*
    fn parse_or(&mut self) -> Result<bool, String> {
        let mut v = self.parse_and()?;
        while self.peek() == Some("-o") {
            self.next();
            let r = self.parse_and()?;
            v = v || r;
        }
        Ok(v)
    }

    /// and := not ( '-a' not )*
    fn parse_and(&mut self) -> Result<bool, String> {
        let mut v = self.parse_not()?;
        while self.peek() == Some("-a") {
            self.next();
            let r = self.parse_not()?;
            v = v && r;
        }
        Ok(v)
    }

    /// not := '!' not | primary
    fn parse_not(&mut self) -> Result<bool, String> {
        if self.peek() == Some("!") {
            self.next();
            return Ok(!self.parse_not()?);
        }
        self.parse_primary()
    }

    /// primary := '(' or ')' | unary ARG | ARG binop ARG | ARG
    fn parse_primary(&mut self) -> Result<bool, String> {
        if self.peek() == Some("(") {
            self.next();
            let v = self.parse_or()?;
            if self.next() != Some(")") {
                return Err("missing ')'".to_string());
            }
            return Ok(v);
        }
        let Some(first) = self.next() else {
            return Err("argument expected".to_string());
        };
        // 一元：-op ARG
        if is_unary_op(first) {
            let Some(arg) = self.next() else {
                return Err(format!("{}: argument expected", first));
            };
            return unary(first, arg).ok_or_else(|| format!("unknown operator: {}", first))?;
        }
        // 二元：ARG op ARG
        if let Some(op) = self.peek()
            && is_binary_op(op)
        {
            self.next();
            let Some(rhs) = self.next() else {
                return Err(format!("{}: argument expected", op));
            };
            return binary(op, first, rhs).ok_or_else(|| format!("unknown operator: {}", op))?;
        }
        // 单参数：非空即真
        Ok(!first.is_empty())
    }
}

/// 求值表达式，返回 Ok(bool) 或 Err(错误信息)。
pub(crate) fn eval(args: &[String]) -> Result<bool, String> {
    if args.is_empty() {
        return Ok(false);
    }
    let mut p = Parser { args, pos: 0 };
    let v = p.parse_or()?;
    if p.pos != args.len() {
        return Err(format!("unexpected argument: {}", args[p.pos]));
    }
    Ok(v)
}

/// `test`/`[` 共用执行逻辑。
fn run_test(args: &[String]) -> ExitCode {
    match eval(args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("test: {}", e);
            ExitCode::from(2)
        }
    }
}

impl Applet for Test {
    fn name(&self) -> &'static str {
        "test"
    }
    fn help(&self) -> &'static str {
        "test EXPR - evaluate conditional expression (files/strings/numbers)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        run_test(args)
    }
}

impl Applet for Bracket {
    fn name(&self) -> &'static str {
        "["
    }
    fn help(&self) -> &'static str {
        "[ EXPR ] - same as test (closing ] required)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        match args.last().map(String::as_str) {
            Some("]") => run_test(&args[..args.len() - 1]),
            _ => {
                eprintln!("[: missing ']'");
                ExitCode::from(2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn name_and_help() {
        assert_eq!(TEST.name(), "test");
        assert_eq!(BRACKET.name(), "[");
        assert!(TEST.help().contains("conditional"));
    }

    #[test]
    fn string_tests() {
        assert_eq!(eval(&s(&["-n", "x"])), Ok(true));
        assert_eq!(eval(&s(&["-n", ""])), Ok(false));
        assert_eq!(eval(&s(&["-z", ""])), Ok(true));
        assert_eq!(eval(&s(&["-z", "x"])), Ok(false));
        assert_eq!(eval(&s(&["abc"])), Ok(true));
        assert_eq!(eval(&s(&[""])), Ok(false));
        assert_eq!(eval(&s(&["a", "=", "a"])), Ok(true));
        assert_eq!(eval(&s(&["a", "!=", "b"])), Ok(true));
    }

    #[test]
    fn numeric_tests() {
        assert_eq!(eval(&s(&["1", "-eq", "1"])), Ok(true));
        assert_eq!(eval(&s(&["1", "-ne", "2"])), Ok(true));
        assert_eq!(eval(&s(&["1", "-lt", "2"])), Ok(true));
        assert_eq!(eval(&s(&["2", "-le", "2"])), Ok(true));
        assert_eq!(eval(&s(&["3", "-gt", "2"])), Ok(true));
        assert_eq!(eval(&s(&["2", "-ge", "3"])), Ok(false));
        assert!(eval(&s(&["x", "-eq", "1"])).is_err());
    }

    #[test]
    fn file_tests() {
        let path = format!("/tmp/rbox_test_expr_{}", std::process::id());
        let _ = fs::write(&path, "data");
        assert_eq!(eval(&s(&["-e", &path])), Ok(true));
        assert_eq!(eval(&s(&["-f", &path])), Ok(true));
        assert_eq!(eval(&s(&["-d", &path])), Ok(false));
        assert_eq!(eval(&s(&["-s", &path])), Ok(true));
        assert_eq!(eval(&s(&["-d", "/tmp"])), Ok(true));
        assert_eq!(eval(&s(&["-e", "/nonexistent_rbox_xyz"])), Ok(false));
        assert_eq!(eval(&s(&[&path, "-nt", "/etc/hostname"])), Ok(true));
        assert_eq!(eval(&s(&[&path, "-ef", &path])), Ok(true));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn logic_operators() {
        assert_eq!(eval(&s(&["!", "-f", "/nonexistent_rbox_xyz"])), Ok(true));
        assert_eq!(
            eval(&s(&["-f", "/etc/hostname", "-a", "-d", "/tmp"])),
            Ok(true)
        );
        assert_eq!(
            eval(&s(&["-f", "/nonexistent_rbox_xyz", "-o", "-d", "/tmp"])),
            Ok(true)
        );
        assert_eq!(
            eval(&s(&[
                "(",
                "-f",
                "/nonexistent_rbox_xyz",
                "-o",
                "-d",
                "/tmp",
                ")"
            ])),
            Ok(true)
        );
    }

    #[test]
    fn errors() {
        assert!(eval(&s(&["-eq", "1"])).is_err());
        assert!(eval(&s(&["(", "1"])).is_err());
        assert!(eval(&s(&["1", "2"])).is_err());
    }

    #[test]
    fn bracket_requires_closing() {
        assert_eq!(BRACKET.run(&s(&["-f", "/etc/hostname"])), ExitCode::from(2));
        assert_eq!(
            BRACKET.run(&s(&["-f", "/etc/hostname", "]"])),
            ExitCode::SUCCESS
        );
        assert_eq!(BRACKET.run(&s(&["-f", "/nope", "]"])), ExitCode::from(1));
    }

    #[test]
    fn empty_is_false() {
        assert_eq!(eval(&[]), Ok(false));
    }
}
