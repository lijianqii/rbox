//! `stat` - 显示文件元数据。
//!
//! 用法：stat [-c FORMAT] FILE...
//! 默认输出类 GNU 风格；`-c` 支持 %n %s %f %a %u %g %i %h %F %y %Y。

use crate::applet::Applet;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Stat;
pub static STAT: &Stat = &Stat;

/// 文件类型描述。
pub(crate) fn file_type(m: &fs::Metadata) -> &'static str {
    let ft = m.file_type();
    if ft.is_dir() {
        "directory"
    } else if ft.is_symlink() {
        "symbolic link"
    } else if ft.is_file() {
        "regular file"
    } else if ft.is_char_device() {
        "character special file"
    } else if ft.is_block_device() {
        "block special file"
    } else if ft.is_fifo() {
        "fifo"
    } else if ft.is_socket() {
        "socket"
    } else {
        "unknown"
    }
}

/// mode -> `rwxrwxrwx` 字符串。
pub(crate) fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// 格式化时间戳（UTC `YYYY-MM-DD HH:MM:SS`）。
pub(crate) fn format_time(secs: i64) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&secs, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// 按 `-c` 格式串渲染。
pub(crate) fn render_format(fmt: &str, path: &str, m: &fs::Metadata) -> String {
    let mtime = m.modified().ok().and_then(|t| {
        t.duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    });
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push_str(path),
            Some('s') => out.push_str(&m.size().to_string()),
            Some('f') => out.push_str(&format!("{:x}", m.mode())),
            Some('a') => out.push_str(&format!("{:o}", m.mode() & 0o7777)),
            Some('u') => out.push_str(&m.uid().to_string()),
            Some('g') => out.push_str(&m.gid().to_string()),
            Some('i') => out.push_str(&m.ino().to_string()),
            Some('h') => out.push_str(&m.nlink().to_string()),
            Some('F') => out.push_str(file_type(m)),
            Some('y') => out.push_str(&mtime.map(format_time).unwrap_or_else(|| "-".into())),
            Some('Y') => out.push_str(&mtime.map(|t| t.to_string()).unwrap_or_else(|| "-".into())),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

impl Applet for Stat {
    fn name(&self) -> &'static str {
        "stat"
    }
    fn help(&self) -> &'static str {
        "stat [-c FORMAT] FILE... - display file metadata"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut format: Option<String> = None;
        let mut files: Vec<&String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-c" | "--format" => {
                    i += 1;
                    let Some(f) = args.get(i) else {
                        eprintln!("stat: option -c requires an argument");
                        return ExitCode::FAILURE;
                    };
                    format = Some(f.clone());
                }
                a if a.starts_with('-') && a.len() > 1 => {
                    eprintln!("stat: unknown option: {}", a);
                    return ExitCode::FAILURE;
                }
                _ => files.push(&args[i]),
            }
            i += 1;
        }
        if files.is_empty() {
            eprintln!("stat: missing operand");
            return ExitCode::FAILURE;
        }
        let mut had_error = false;
        for f in files {
            let m = match fs::symlink_metadata(f) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("stat: {}: {}", f, e);
                    had_error = true;
                    continue;
                }
            };
            if let Some(fmt) = &format {
                println!("{}", render_format(fmt, f, &m));
                continue;
            }
            let secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            println!("  File: {}", f);
            println!(
                "  Size: {:<10} Blocks: {:<8} IO Block: {:<6} {}",
                m.size(),
                m.blocks(),
                4096,
                file_type(&m)
            );
            println!(
                "Device: {:<8} Inode: {:<10} Links: {}",
                m.dev(),
                m.ino(),
                m.nlink()
            );
            println!(
                "Access: ({:04o}/{})  Uid: ({})  Gid: ({})",
                m.mode() & 0o7777,
                mode_string(m.mode()),
                m.uid(),
                m.gid()
            );
            println!("Modify: {}", format_time(secs));
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
        assert_eq!(STAT.name(), "stat");
        assert!(STAT.help().contains("metadata"));
    }

    #[test]
    fn mode_string_format() {
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o644), "rw-r--r--");
        assert_eq!(mode_string(0o000), "---------");
    }

    #[test]
    fn format_codes() {
        let path = format!("/tmp/rbox_stat_{}", std::process::id());
        fs::write(&path, "hello").unwrap();
        let m = fs::metadata(&path).unwrap();
        let out = render_format("%n %s %a %F", &path, &m);
        assert!(out.starts_with(&path), "{}", out);
        assert!(out.contains(" 5 "), "{}", out);
        assert!(out.ends_with("regular file"), "{}", out);
        let out = render_format("%%literal", &path, &m);
        assert_eq!(out, "%literal");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_fails() {
        assert_eq!(
            STAT.run(&["/nonexistent_stat_xyz".to_string()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn default_output_succeeds() {
        assert_eq!(STAT.run(&["/etc/hostname".to_string()]), ExitCode::SUCCESS);
    }
}
