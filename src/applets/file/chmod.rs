//! `chmod` - 修改文件权限。
//!
//! 用法：chmod MODE FILE...
//!       chmod [-R] MODE FILE...
//!
//! MODE 支持八进制（如 755、0644）与符号形式（如 u+x、go-w、a=r、u=rw,go=r）。
//! `-R` 递归处理目录（符号链接只改链接本身，不跟随）。

use crate::applet::Applet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::ExitCode;

pub struct Chmod;
pub static CHMOD: &Chmod = &Chmod;

/// 解析八进制权限（1-4 位，0-7）。
pub(crate) fn parse_octal(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return None;
    }
    u32::from_str_radix(s, 8).ok()
}

/// 应用一段符号权限（可含逗号分隔的多段）到当前 mode。
/// `is_dir` 用于 `X` 语义（目录或已有执行位时按 x 处理）。
pub(crate) fn apply_symbolic(mode: u32, is_dir: bool, spec: &str) -> Option<u32> {
    let mut mode = mode;
    for clause in spec.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            return None;
        }
        let mut chars = clause.chars().peekable();
        let mut who = 0u32;
        let mut explicit_who = false;
        while let Some(&c) = chars.peek() {
            match c {
                'u' => who |= 0o700,
                'g' => who |= 0o070,
                'o' => who |= 0o007,
                'a' => who |= 0o777,
                _ => break,
            }
            explicit_who = true;
            chars.next();
        }
        if !explicit_who {
            who = 0o777;
        }
        let op = chars.next()?;
        let mut bits = 0u32;
        let mut special = 0u32;
        for c in chars {
            match c {
                'r' => bits |= 0o444,
                'w' => bits |= 0o222,
                'x' => bits |= 0o111,
                'X' => {
                    if is_dir || mode & 0o111 != 0 {
                        bits |= 0o111;
                    }
                }
                's' => {
                    if who & 0o700 != 0 {
                        special |= 0o4000;
                    }
                    if who & 0o070 != 0 {
                        special |= 0o2000;
                    }
                }
                't' => special |= 0o1000,
                _ => return None,
            }
        }
        let bits = (bits & who) | special;
        let mask = who | 0o7000;
        match op {
            '+' => mode |= bits,
            '-' => mode &= !bits,
            '=' => {
                mode &= !mask;
                mode |= bits;
            }
            _ => return None,
        }
    }
    Some(mode)
}

/// 递归应用权限（符号链接只改链接本身，使用 `symlink_metadata` 判断）。
fn chmod_recursive(path: &str, mode: u32, recursive: bool, had_error: &mut bool) {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("chmod: {}: {}", path, e);
            *had_error = true;
            return;
        }
    };
    if meta.file_type().is_symlink() {
        // 不跟随符号链接（与 busybox 一致）
        return;
    }
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        eprintln!("chmod: {}: {}", path, e);
        *had_error = true;
    }
    if recursive && meta.is_dir() {
        match fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let sub = entry.path();
                    chmod_recursive(&sub.to_string_lossy(), mode, true, had_error);
                }
            }
            Err(e) => {
                eprintln!("chmod: {}: {}", path, e);
                *had_error = true;
            }
        }
    }
}

impl Applet for Chmod {
    fn name(&self) -> &'static str {
        "chmod"
    }
    fn help(&self) -> &'static str {
        "chmod [-R] MODE FILE... - change file permission bits"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut recursive = false;
        let mut end_of_options = false;
        let mut rest: Vec<&String> = Vec::new();
        for a in args {
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        continue;
                    }
                    "-R" | "-r" | "--recursive" => {
                        recursive = true;
                        continue;
                    }
                    _ => {}
                }
            }
            rest.push(a);
        }
        if rest.len() < 2 {
            eprintln!("chmod: usage: chmod [-R] MODE FILE...");
            return ExitCode::FAILURE;
        }
        let spec = rest[0].as_str();
        let files = &rest[1..];

        // 先验证 mode（避免处理到一半才发现非法）
        let octal = parse_octal(spec);
        if octal.is_none() && apply_symbolic(0o644, false, spec).is_none() {
            eprintln!("chmod: invalid mode: '{}'", spec);
            return ExitCode::FAILURE;
        }

        let mut had_error = false;
        for file in files {
            // 每个文件单独解析符号权限（X 依赖当前 mode）
            let mode = match octal {
                Some(m) => m,
                None => {
                    let is_dir = fs::metadata(file).map(|m| m.is_dir()).unwrap_or(false);
                    let current = match fs::symlink_metadata(file) {
                        Ok(m) => m.permissions().mode(),
                        Err(e) => {
                            eprintln!("chmod: {}: {}", file, e);
                            had_error = true;
                            continue;
                        }
                    };
                    apply_symbolic(current, is_dir, spec).unwrap_or(current)
                }
            };
            chmod_recursive(file, mode, recursive, &mut had_error);
        }
        if had_error {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(CHMOD.name(), "chmod");
        assert!(CHMOD.help().contains("permission"));
    }

    #[test]
    fn octal_parsing() {
        assert_eq!(parse_octal("755"), Some(0o755));
        assert_eq!(parse_octal("0644"), Some(0o644));
        assert_eq!(parse_octal("0"), Some(0));
        assert_eq!(parse_octal("7777"), Some(0o7777));
        assert_eq!(parse_octal("8"), None);
        assert_eq!(parse_octal(""), None);
        assert_eq!(parse_octal("12345"), None);
    }

    #[test]
    fn symbolic_add_remove_set() {
        assert_eq!(apply_symbolic(0o644, false, "u+x"), Some(0o744));
        assert_eq!(apply_symbolic(0o777, false, "go-w"), Some(0o755));
        assert_eq!(apply_symbolic(0o700, false, "a=r"), Some(0o444));
        assert_eq!(apply_symbolic(0o644, false, "u=rw,go=r"), Some(0o644));
        assert_eq!(apply_symbolic(0o000, false, "a+rwx"), Some(0o777));
    }

    #[test]
    fn symbolic_capital_x_needs_exec_or_dir() {
        assert_eq!(apply_symbolic(0o644, false, "a+X"), Some(0o644));
        assert_eq!(apply_symbolic(0o744, false, "a+X"), Some(0o755));
        assert_eq!(apply_symbolic(0o644, true, "a+X"), Some(0o755));
    }

    #[test]
    fn symbolic_special_bits() {
        assert_eq!(apply_symbolic(0o755, false, "u+s"), Some(0o4755));
        assert_eq!(apply_symbolic(0o755, false, "g+s"), Some(0o2755));
        assert_eq!(apply_symbolic(0o755, false, "+t"), Some(0o1755));
        assert_eq!(apply_symbolic(0o4755, false, "u-s"), Some(0o755));
    }

    #[test]
    fn symbolic_invalid() {
        assert_eq!(apply_symbolic(0o644, false, "u+q"), None);
        assert_eq!(apply_symbolic(0o644, false, "u"), None);
        assert_eq!(apply_symbolic(0o644, false, ""), None);
    }

    #[test]
    fn chmod_run_on_temp_file() {
        let path = format!("/tmp/rbox_chmod_test_{}", std::process::id());
        fs::write(&path, "x").unwrap();
        let args = vec!["600".to_string(), path.clone()];
        assert_eq!(CHMOD.run(&args), ExitCode::SUCCESS);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn chmod_missing_args_fails() {
        assert_eq!(CHMOD.run(&[]), ExitCode::FAILURE);
        assert_eq!(CHMOD.run(&["755".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn chmod_invalid_mode_fails() {
        let path = format!("/tmp/rbox_chmod_bad_{}", std::process::id());
        fs::write(&path, "x").unwrap();
        let args = vec!["u+q".to_string(), path.clone()];
        assert_eq!(CHMOD.run(&args), ExitCode::FAILURE);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn chmod_symbolic_and_double_dash() {
        let path = format!("/tmp/rbox_chmod_sym_{}", std::process::id());
        fs::write(&path, "x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let args = vec!["u+x".to_string(), "--".to_string(), path.clone()];
        assert_eq!(CHMOD.run(&args), ExitCode::SUCCESS);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = fs::remove_file(&path);
    }
}
