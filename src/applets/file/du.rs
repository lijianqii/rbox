//! `du` - 统计目录/文件磁盘占用。
//!
//! 用法：du [-s] [-h] [path...]
//! - 默认递归列出每个目录的累计大小（KB）；`-s` 只显示总计；`-h` 人类可读。

use crate::applet::Applet;
use crate::applets::proc::human_size;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

pub struct Du;
pub static DU: &Du = &Du;

/// 递归累计大小（KB），并收集 `(路径, KB)` 明细。
pub(crate) fn du_walk(path: &Path, out: &mut Vec<(String, u64)>) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let mut total: u64 = meta.len();
    if meta.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        let mut children: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            total += du_walk(&child, out);
        }
    }
    out.push((path.to_string_lossy().into_owned(), total.div_ceil(1024)));
    total
}

/// 格式化大小：`-h` 人类可读，否则 `N\tpath`（KB）。
fn format_line(path: &str, kb: u64, human: bool) -> String {
    if human {
        format!("{}\t{}", human_size(kb * 1024), path)
    } else {
        format!("{}\t{}", kb, path)
    }
}

impl Applet for Du {
    fn name(&self) -> &'static str {
        "du"
    }
    fn help(&self) -> &'static str {
        "du [-s] [-h] [path...] - estimate file space usage"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut summary = false;
        let mut human = false;
        let mut paths: Vec<String> = Vec::new();
        let mut end_of_options = false;
        for a in args {
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        continue;
                    }
                    "-s" | "--summarize" => {
                        summary = true;
                        continue;
                    }
                    "-h" | "--human-readable" => {
                        human = true;
                        continue;
                    }
                    s if s.starts_with('-') && s.len() > 1 => {
                        eprintln!("du: unknown option: {}", s);
                        return ExitCode::FAILURE;
                    }
                    _ => {}
                }
            }
            paths.push(a.clone());
        }
        if paths.is_empty() {
            paths.push(".".to_string());
        }
        let mut had_error = false;
        for p in &paths {
            let mut entries = Vec::new();
            let total = du_walk(Path::new(p), &mut entries);
            if total == 0 && !Path::new(p).exists() {
                eprintln!("du: {}: No such file or directory", p);
                had_error = true;
                continue;
            }
            if summary {
                println!("{}", format_line(p, total.div_ceil(1024), human));
            } else {
                for (path, kb) in &entries {
                    println!("{}", format_line(path, *kb, human));
                }
            }
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
        assert_eq!(DU.name(), "du");
        assert!(DU.help().contains("space"));
    }

    #[test]
    fn walk_sums_sizes() {
        let dir = format!("/tmp/rbox_du_{}", std::process::id());
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(format!("{}/sub", dir)).unwrap();
        fs::write(format!("{}/a", dir), vec![0u8; 2048]).unwrap();
        fs::write(format!("{}/sub/b", dir), vec![0u8; 1024]).unwrap();
        let mut entries = Vec::new();
        let total = du_walk(Path::new(&dir), &mut entries);
        // 目录自身 st_size 因文件系统而异，只校验文件总和被包含
        assert!(total >= 3072, "total={}", total);
        assert!(entries.iter().any(|(p, kb)| p.ends_with("/a") && *kb == 2));
        assert!(
            entries
                .iter()
                .any(|(p, kb)| p.ends_with("/sub") && *kb >= 1)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_path_fails() {
        assert_eq!(
            DU.run(&["/nonexistent_du_xyz".to_string()]),
            ExitCode::FAILURE
        );
    }
}
