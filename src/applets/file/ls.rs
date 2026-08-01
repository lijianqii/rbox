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
            if !show_all && name.starts_with('.') {
                continue;
            }
            entries.push(name);
        }
        entries.sort();

        if long {
            for name in &entries {
                let full = Path::new(path).join(name);
                let m = fs::metadata(&full)?;
                print_long(name, &m);
            }
        } else if one {
            for name in &entries {
                println!("{}", name);
            }
        } else {
            // 横向输出，空格分隔
            let line = entries.join("  ");
            if !line.is_empty() {
                println!("{}", line);
            }
        }
    } else {
        // 普通文件
        if long {
            let name = Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string());
            print_long(&name, &meta);
        } else {
            println!("{}", path);
        }
    }
    Ok(())
}

fn print_long(name: &str, m: &fs::Metadata) {
    let mode = mode_string(m);
    let nlink = m.nlink();
    let uid = m.uid();
    let gid = m.gid();
    let size = m.size();
    let mtime = m.mtime();
    let time_str = format_time(mtime);
    println!(
        "{} {} {} {} {:>8} {} {}",
        mode, nlink, uid, gid, size, time_str, name
    );
}

fn mode_string(m: &fs::Metadata) -> String {
    let mode = m.mode();
    let ft = m.file_type();
    let mut s = String::with_capacity(10);
    s.push(if ft.is_dir() {
        'd'
    } else if ft.is_symlink() {
        'l'
    } else {
        '-'
    });
    // owner
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    // group
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    // other
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    s
}

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
