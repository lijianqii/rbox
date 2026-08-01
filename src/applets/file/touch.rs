//! `touch` - 更新文件时间戳或创建空文件。
//!
//! 用法：touch FILES...
//! 若文件不存在则创建；若存在则更新访问/修改时间为当前时间。

use crate::applet::Applet;
use std::ffi::CString;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Touch;
pub static TOUCH: &Touch = &Touch;

impl Applet for Touch {
    fn name(&self) -> &'static str {
        "touch"
    }
    fn help(&self) -> &'static str {
        "touch FILES... - update timestamps or create files"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let files: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if files.is_empty() {
            eprintln!("touch: missing operand");
            return ExitCode::FAILURE;
        }

        let mut had_error = false;
        for f in &files {
            if let Err(e) = touch_one(f) {
                eprintln!("touch: {}: {}", f, e);
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

fn touch_one(path: &str) -> io::Result<()> {
    // 确保文件存在（目录跳过创建，仅更新时间戳）
    match OpenOptions::new().create(true).write(true).open(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::IsADirectory => {}
        Err(e) => return Err(e),
    }

    // 设置修改/访问时间为当前时间
    set_file_times(path, SystemTime::now(), SystemTime::now())
}

/// 设置文件的访问和修改时间（使用 libc::utimensat）。
fn set_file_times(path: &str, atime: SystemTime, mtime: SystemTime) -> io::Result<()> {
    let c_path = CString::new(Path::new(path).as_os_str().as_bytes())?;
    let atv = to_timespec(atime)?;
    let mtv = to_timespec(mtime)?;

    let times = [
        libc::timespec {
            tv_sec: atv.0,
            tv_nsec: atv.1 as i64,
        },
        libc::timespec {
            tv_sec: mtv.0,
            tv_nsec: mtv.1 as i64,
        },
    ];

    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn to_timespec(t: SystemTime) -> io::Result<(i64, i64)> {
    let dur = t
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "time before UNIX epoch"))?;
    Ok((dur.as_secs() as i64, dur.subsec_nanos() as i64))
}
