//! `find` - 递归查找文件。
//!
//! 用法：find [PATH...] [-name PATTERN] [-type f|d]
//! - 无 PATH 时从当前目录 `.` 开始；
//! - `-name`：按文件名 glob 匹配（`*` `?` `[]`）；
//! - `-type f|d`：只输出普通文件 / 目录（不跟随符号链接）。

use crate::applet::Applet;
use crate::applets::glob::glob_match;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub struct Find;
pub static FIND: &Find = &Find;

/// 查找条件。
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct FindOpts {
    pub(crate) name: Option<String>,
    pub(crate) typ: Option<char>,
    /// 最大递归深度（根为 0；None = 不限）
    pub(crate) maxdepth: Option<usize>,
}

/// 解析参数：路径列表 + 条件。未知选项返回 Err。
pub(crate) fn parse_args(args: &[String]) -> Result<(Vec<String>, FindOpts), String> {
    let mut paths: Vec<String> = Vec::new();
    let mut opts = FindOpts::default();
    let mut i = 0;
    let mut end_of_options = false;
    while i < args.len() {
        let arg = args[i].as_str();
        if end_of_options {
            paths.push(arg.to_string());
            i += 1;
            continue;
        }
        match arg {
            "--" => {
                end_of_options = true;
            }
            "-name" => {
                i += 1;
                let Some(p) = args.get(i) else {
                    return Err("-name requires a pattern".to_string());
                };
                opts.name = Some(p.clone());
            }
            "-type" => {
                i += 1;
                let Some(t) = args.get(i) else {
                    return Err("-type requires an argument".to_string());
                };
                match t.as_str() {
                    "f" | "d" => opts.typ = t.chars().next(),
                    other => return Err(format!("unknown type '{}'", other)),
                }
            }
            "-maxdepth" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Err("-maxdepth requires a number".to_string());
                };
                match v.parse::<usize>() {
                    Ok(n) => opts.maxdepth = Some(n),
                    Err(_) => return Err(format!("invalid maxdepth '{}'", v)),
                }
            }
            "-print" => {} // 默认行为，显式接受
            a if a.starts_with('-') && a.len() > 1 => {
                return Err(format!("unknown option '{}'", a));
            }
            p => paths.push(p.to_string()),
        }
        i += 1;
    }
    if paths.is_empty() {
        paths.push(".".to_string());
    }
    Ok((paths, opts))
}

/// 判断条目是否满足条件。
pub(crate) fn entry_matches(meta: &fs::Metadata, file_name: &str, opts: &FindOpts) -> bool {
    if let Some(t) = opts.typ {
        let ok = match t {
            'f' => meta.is_file(),
            'd' => meta.is_dir(),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    if let Some(pattern) = &opts.name
        && !glob_match(pattern, file_name)
    {
        return false;
    }
    true
}

/// 递归遍历（不跟随符号链接），把命中的路径追加到 out。
fn walk(path: &Path, depth: usize, opts: &FindOpts, out: &mut Vec<String>, had_error: &mut bool) {
    if let Some(max) = opts.maxdepth
        && depth > max
    {
        return;
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("find: {}: {}", path.display(), e);
            *had_error = true;
            return;
        }
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    if entry_matches(&meta, &name, opts) {
        out.push(path.to_string_lossy().into_owned());
    }
    // 只递归真实目录（symlink_metadata 已保证符号链接不被跟随）
    if meta.is_dir() {
        match fs::read_dir(path) {
            Ok(entries) => {
                let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
                children.sort();
                for child in children {
                    walk(&child, depth + 1, opts, out, had_error);
                }
            }
            Err(e) => {
                eprintln!("find: {}: {}", path.display(), e);
                *had_error = true;
            }
        }
    }
}

/// 执行查找，返回 (命中路径, 是否有错误)。
pub(crate) fn run_find(roots: &[String], opts: &FindOpts) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut had_error = false;
    for root in roots {
        walk(Path::new(root), 0, opts, &mut out, &mut had_error);
    }
    (out, had_error)
}

impl Applet for Find {
    fn name(&self) -> &'static str {
        "find"
    }
    fn help(&self) -> &'static str {
        "find [PATH...] [-name PATTERN] [-type f|d] - search files recursively"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let (paths, opts) = match parse_args(args) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("find: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let (hits, had_error) = run_find(&paths, &opts);
        for h in hits {
            println!("{}", h);
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

    fn tmpdir(tag: &str) -> String {
        let dir = format!("/tmp/rbox_find_{}_{}", tag, std::process::id());
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(format!("{}/sub/deep", dir)).unwrap();
        fs::write(format!("{}/a.txt", dir), "a").unwrap();
        fs::write(format!("{}/b.log", dir), "b").unwrap();
        fs::write(format!("{}/sub/c.txt", dir), "c").unwrap();
        fs::write(format!("{}/sub/deep/d.txt", dir), "d").unwrap();
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(FIND.name(), "find");
        assert!(FIND.help().contains("recursively"));
    }

    #[test]
    fn parse_defaults_to_cwd() {
        let (paths, opts) = parse_args(&[]).unwrap();
        assert_eq!(paths, vec!["."]);
        assert_eq!(opts, FindOpts::default());
    }

    #[test]
    fn parse_name_and_type() {
        let args = vec![
            "/tmp".to_string(),
            "-name".to_string(),
            "*.txt".to_string(),
            "-type".to_string(),
            "f".to_string(),
        ];
        let (paths, opts) = parse_args(&args).unwrap();
        assert_eq!(paths, vec!["/tmp"]);
        assert_eq!(opts.name.as_deref(), Some("*.txt"));
        assert_eq!(opts.typ, Some('f'));
    }

    #[test]
    fn parse_errors() {
        assert!(parse_args(&["-name".to_string()]).is_err());
        assert!(parse_args(&["-type".to_string()]).is_err());
        assert!(parse_args(&["-type".to_string(), "x".to_string()]).is_err());
        assert!(parse_args(&["-bogus".to_string()]).is_err());
        assert!(parse_args(&["-maxdepth".to_string()]).is_err());
        assert!(parse_args(&["-maxdepth".to_string(), "x".to_string()]).is_err());
    }

    #[test]
    fn parse_maxdepth_and_print() {
        let (paths, opts) = parse_args(&[
            "/tmp".to_string(),
            "-maxdepth".to_string(),
            "2".to_string(),
            "-print".to_string(),
        ])
        .unwrap();
        assert_eq!(paths, vec!["/tmp"]);
        assert_eq!(opts.maxdepth, Some(2));
    }

    #[test]
    fn parse_double_dash_allows_dash_path() {
        let (paths, _) = parse_args(&["--".to_string(), "-weird-dir".to_string()]).unwrap();
        assert_eq!(paths, vec!["-weird-dir"]);
    }

    #[test]
    fn finds_by_name_recursively() {
        let dir = tmpdir("name");
        let (paths, opts) =
            parse_args(&[dir.clone(), "-name".to_string(), "*.txt".to_string()]).unwrap();
        let (hits, err) = run_find(&paths, &opts);
        assert!(!err);
        assert_eq!(hits.len(), 3, "hits: {:?}", hits);
        assert!(hits.iter().all(|h| h.ends_with(".txt")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_only_directories() {
        let dir = tmpdir("dir");
        let (paths, opts) = parse_args(&[
            dir.clone(),
            "-type".to_string(),
            "d".to_string(),
            "-name".to_string(),
            "deep".to_string(),
        ])
        .unwrap();
        let (hits, err) = run_find(&paths, &opts);
        assert!(!err);
        assert_eq!(hits, vec![format!("{}/sub/deep", dir)]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_root_reports_error() {
        let (paths, opts) = parse_args(&["/nonexistent_rbox_find".to_string()]).unwrap();
        let (hits, err) = run_find(&paths, &opts);
        assert!(err);
        assert!(hits.is_empty());
    }

    #[test]
    fn maxdepth_limits_recursion() {
        let dir = tmpdir("depth");
        let (paths, opts) = parse_args(&[
            dir.clone(),
            "-name".to_string(),
            "*.txt".to_string(),
            "-maxdepth".to_string(),
            "1".to_string(),
        ])
        .unwrap();
        let (hits, err) = run_find(&paths, &opts);
        assert!(!err);
        // depth 0: dir 本身（非 .txt）；depth 1: a.txt；sub/c.txt 在 depth 2 被截断
        assert_eq!(hits, vec![format!("{}/a.txt", dir)]);
        // 不限深度时应找到 3 个 .txt
        let (all, _) = run_find(
            std::slice::from_ref(&dir),
            &FindOpts {
                name: Some("*.txt".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(all.len(), 3, "hits: {:?}", all);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_option_fails() {
        assert_eq!(FIND.run(&["-bogus".to_string()]), ExitCode::FAILURE);
    }
}
