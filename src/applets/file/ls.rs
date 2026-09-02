//! `ls` - 列出目录内容。
//!
//! 选项：
//!   -a   显示隐藏文件（以 . 开头）
//!   -l   长格式（权限、链接数、所有者、组、大小、时间、名称）
//!   -1   每行一个
//!   默认横向多列输出。

use crate::applet::Applet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

pub struct Ls;
pub static LS: &Ls = &Ls;

impl Applet for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }
    fn help(&self) -> &'static str {
        "ls [-a] [-l] [-1] [files...] - list directory contents"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut show_all = false;
        let mut long = false;
        let mut one_per_line = false;
        let mut paths: Vec<&str> = Vec::new();

        for a in args {
            if a.starts_with('-') && a.len() > 1 {
                for c in a[1..].chars() {
                    match c {
                        'a' => show_all = true,
                        'l' => long = true,
                        '1' => one_per_line = true,
                        _ => {}
                    }
                }
            } else {
                paths.push(a);
            }
        }

        if paths.is_empty() {
            paths.push(".");
        }

        let mut had_error = false;
        let multi = paths.len() > 1;

        for (i, p) in paths.iter().enumerate() {
            if multi {
                if i > 0 {
                    println!();
                }
                println!("{}:", p);
            }
            if let Err(e) = list_path(p, show_all, long, one_per_line) {
                eprintln!("ls: {}: {}", p, e);
                had_error = true;
            }
        }

        if had_error {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

fn list_path(path: &str, show_all: bool, long: bool, one: bool) -> std::io::Result<()> {
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        let mut entries: Vec<String> = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push(name);
        }
        entries = filter_entries(entries, show_all);

        if long {
            let items = collect_metadata(path, &entries)?;
            for line in format_long(&items) {
                println!("{}", line);
            }
        } else if one {
            for name in &entries {
                println!("{}", name);
            }
        } else {
            // 默认多列对齐输出（按终端宽度分列）
            for line in format_columns(&entries, terminal_width()) {
                println!("{}", line);
            }
        }
    } else {
        // 普通文件/符号链接
        if long {
            let lmeta = fs::symlink_metadata(path)?;
            let name = Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string());
            let target = symlink_target(Path::new(path), &lmeta);
            for line in format_long(&[(name, lmeta, target)]) {
                println!("{}", line);
            }
        } else {
            println!("{}", path);
        }
    }
    Ok(())
}

/// 收集目录条目的 (名称, metadata, 符号链接目标)。
fn collect_metadata(
    path: &str,
    entries: &[String],
) -> std::io::Result<Vec<(String, fs::Metadata, Option<String>)>> {
    let mut items = Vec::with_capacity(entries.len());
    for name in entries {
        let full = Path::new(path).join(name);
        // 使用 symlink_metadata，避免跟随符号链接后丢失链接类型
        let m = fs::symlink_metadata(&full)?;
        let target = symlink_target(&full, &m);
        items.push((name.clone(), m, target));
    }
    Ok(items)
}

/// 若文件是符号链接，返回其目标字符串。
fn symlink_target(path: &Path, meta: &fs::Metadata) -> Option<String> {
    if meta.file_type().is_symlink() {
        fs::read_link(path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// 查询 stdout 终端宽度；非 tty 时回退 80 列。
fn terminal_width() -> usize {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            ws.ws_col as usize
        } else {
            80
        }
    }
}

/// 按列优先（column-major）将条目排版为多行：列宽 = 最长名 + 2 空格，
/// 列数由终端宽度决定（至少 1 列）。
fn format_columns(entries: &[String], term_width: usize) -> Vec<String> {
    if entries.is_empty() {
        return Vec::new();
    }
    let max_len = entries.iter().map(|s| s.len()).max().unwrap_or(0);
    let col_width = max_len + 2;
    let ncols = (term_width / col_width.max(1)).max(1);
    let nrows = entries.len().div_ceil(ncols);
    let mut lines = Vec::with_capacity(nrows);
    for r in 0..nrows {
        let mut line = String::new();
        for c in 0..ncols {
            let idx = c * nrows + r;
            if idx >= entries.len() {
                break;
            }
            if c > 0 {
                let target = col_width * c;
                if line.len() < target {
                    line.push_str(&" ".repeat(target - line.len()));
                }
            }
            line.push_str(&entries[idx]);
        }
        lines.push(line);
    }
    lines
}

/// 长格式：nlink/uid/gid/size 按各自列的最大宽度右对齐。
/// 符号链接额外追加 `-> target`。
fn format_long(items: &[(String, fs::Metadata, Option<String>)]) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }
    let nlink_w = items
        .iter()
        .map(|(_, m, _)| m.nlink().to_string().len())
        .max()
        .unwrap_or(1);
    let uid_w = items
        .iter()
        .map(|(_, m, _)| m.uid().to_string().len())
        .max()
        .unwrap_or(1);
    let gid_w = items
        .iter()
        .map(|(_, m, _)| m.gid().to_string().len())
        .max()
        .unwrap_or(1);
    let size_w = items
        .iter()
        .map(|(_, m, _)| m.size().to_string().len())
        .max()
        .unwrap_or(1);
    items
        .iter()
        .map(|(name, m, target)| {
            let mut line = format!(
                "{} {:>nw$} {:>uw$} {:>gw$} {:>sw$} {} {}",
                mode_string(m),
                m.nlink(),
                m.uid(),
                m.gid(),
                m.size(),
                format_time(m.mtime()),
                name,
                nw = nlink_w,
                uw = uid_w,
                gw = gid_w,
                sw = size_w,
            );
            if let Some(t) = target {
                line.push_str(" -> ");
                line.push_str(t);
            }
            line
        })
        .collect()
}

/// 将文件 mode 转为 `drwxr-xr-x` 格式字符串。
fn mode_string_from_mode(mode: u32, is_dir: bool, is_symlink: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir {
        'd'
    } else if is_symlink {
        'l'
    } else {
        '-'
    });
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    s
}

fn mode_string(m: &fs::Metadata) -> String {
    mode_string_from_mode(m.mode(), m.file_type().is_dir(), m.file_type().is_symlink())
}

/// 过滤目录条目：去除隐藏文件（除非 show_all）。
fn filter_entries(mut entries: Vec<String>, show_all: bool) -> Vec<String> {
    if !show_all {
        entries.retain(|n| !n.starts_with('.'));
    }
    entries.sort();
    entries
}

/// 将 mtime 格式化为 `Mon DD HH:MM`（ctime 风格）。
fn format_time(mtime: i64) -> String {
    // 简单格式化：使用 ctime 风格的简短形式 "Mon DD HH:MM"
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = mtime;
    unsafe { libc::localtime_r(&t as *const i64, &mut tm) };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mon = months.get(tm.tm_mon as usize).copied().unwrap_or("???");
    format!(
        "{} {:>2} {:02}:{:02}",
        mon, tm.tm_mday, tm.tm_hour, tm.tm_min
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_regular_file() {
        assert_eq!(mode_string_from_mode(0o644, false, false), "-rw-r--r--");
    }

    #[test]
    fn mode_executable() {
        assert_eq!(mode_string_from_mode(0o755, false, false), "-rwxr-xr-x");
    }

    #[test]
    fn mode_directory() {
        assert_eq!(mode_string_from_mode(0o755, true, false), "drwxr-xr-x");
    }

    #[test]
    fn mode_symlink() {
        assert_eq!(mode_string_from_mode(0o777, false, true), "lrwxrwxrwx");
    }

    #[test]
    fn mode_no_permissions() {
        assert_eq!(mode_string_from_mode(0o000, false, false), "----------");
    }

    #[test]
    fn filter_hidden() {
        let entries = vec![".hidden".into(), "visible".into(), ".secret".into()];
        assert_eq!(filter_entries(entries, false), vec!["visible"]);
    }

    #[test]
    fn filter_show_all() {
        let entries = vec![".hidden".into(), "visible".into(), ".secret".into()];
        assert_eq!(
            filter_entries(entries, true),
            vec![".hidden", ".secret", "visible"]
        );
    }

    #[test]
    fn filter_empty() {
        let entries: Vec<String> = vec![];
        assert_eq!(filter_entries(entries, false), Vec::<String>::new());
    }

    #[test]
    fn columns_empty() {
        assert!(format_columns(&[], 80).is_empty());
    }

    #[test]
    fn columns_single_column() {
        let entries = vec!["a".into(), "bb".into(), "ccc".into()];
        // col_width = 3+2 = 5 > term_width 3，只有 1 列
        let lines = format_columns(&entries, 3);
        assert_eq!(lines, vec!["a", "bb", "ccc"]);
    }

    #[test]
    fn columns_multi_column_aligned() {
        let entries = vec![
            "aaa".into(),
            "bbb".into(),
            "ccc".into(),
            "dddd".into(),
            "eee".into(),
            "fff".into(),
        ];
        // max_len=4, col_width=6, term_width=20 => 3 列 2 行，column-major
        let lines = format_columns(&entries, 20);
        assert_eq!(lines[0], "aaa   ccc   eee");
        assert_eq!(lines[1], "bbb   dddd  fff");
    }

    #[test]
    fn long_format_aligns_columns() {
        let dir = std::env::temp_dir().join(format!("rbox_ls_long_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("a");
        let f2 = dir.join("b");
        std::fs::write(&f1, "12345").unwrap(); // size 列宽 5
        std::fs::write(&f2, "1").unwrap();
        let items = vec![
            (
                "a".to_string(),
                std::fs::symlink_metadata(&f1).unwrap(),
                None,
            ),
            (
                "b".to_string(),
                std::fs::symlink_metadata(&f2).unwrap(),
                None,
            ),
        ];
        let lines = format_long(&items);
        assert_eq!(lines.len(), 2);
        // 两行 name 均为 1 字符；size 列按最大宽度 5 右对齐 ⟹ 两行总长相等
        assert_eq!(lines[0].len(), lines[1].len(), "列未对齐: {:?}", lines);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_format_symlink_arrow() {
        let dir = std::env::temp_dir().join(format!("rbox_ls_symlink_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target");
        let link = dir.join("link");
        std::fs::write(&target, "x").unwrap();
        std::os::unix::fs::symlink("target", &link).unwrap();
        let m = std::fs::symlink_metadata(&link).unwrap();
        let target_str = std::fs::read_link(&link)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let lines = format_long(&[("link".to_string(), m, Some(target_str))]);
        assert!(lines[0].contains("lrwxrwxrwx"), "{}", lines[0]);
        assert!(lines[0].contains("-> target"), "{}", lines[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
