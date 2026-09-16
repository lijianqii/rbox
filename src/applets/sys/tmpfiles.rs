//! `tmpfiles` - 按 tmpfiles.d 风格配置创建/清理临时目录与文件。
//!
//! 用法：`rbox tmpfiles [CONF...]`
//! 缺省读取 `/etc/rbox/tmpfiles.d/*.conf` 与 `/usr/lib/rbox/tmpfiles.d/*.conf`（字典序）。
//! 支持行类型：`d` 目录、`f` 文件（可带内容）、`L` 符号链接、`r` 删除、`R` 递归删除、
//! `w` 写入内容、`z` 调整现有路径权限；未知类型告警跳过。
//! 字段：`TYPE PATH [MODE] [USER] [GROUP] [AGE] [ARG]`（`-` 表示缺省）。

use crate::applet::Applet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

pub struct Tmpfiles;
pub static TMPFILES: &Tmpfiles = &Tmpfiles;

impl Applet for Tmpfiles {
    fn name(&self) -> &'static str {
        "tmpfiles"
    }
    fn help(&self) -> &'static str {
        "tmpfiles [CONF...] - create/clean temp dirs and files from tmpfiles.d style configs"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let files: Vec<String> = if args.is_empty() {
            default_configs()
        } else {
            args.to_vec()
        };
        if files.is_empty() {
            eprintln!("tmpfiles: no config files found");
            return ExitCode::FAILURE;
        }
        let mut rc = ExitCode::SUCCESS;
        for f in &files {
            let Ok(content) = std::fs::read_to_string(f) else {
                eprintln!("tmpfiles: cannot read {}", f);
                rc = ExitCode::FAILURE;
                continue;
            };
            for (no, raw) in content.lines().enumerate() {
                let line = raw.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if !apply_line(line) {
                    eprintln!(
                        "tmpfiles: {}:{}: unsupported or failed: {}",
                        f,
                        no + 1,
                        line
                    );
                    rc = ExitCode::FAILURE;
                }
            }
        }
        rc
    }
}

/// 缺省配置：/etc/rbox/tmpfiles.d 与 /usr/lib/rbox/tmpfiles.d 下的 *.conf（字典序）。
fn default_configs() -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["/usr/lib/rbox/tmpfiles.d", "/etc/rbox/tmpfiles.d"] {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut paths: Vec<String> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"))
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            paths.sort();
            out.extend(paths);
        }
    }
    out
}

/// 应用一行配置；返回是否成功（未知类型返回 false）。
pub(crate) fn apply_line(line: &str) -> bool {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return false;
    }
    let typ = parts[0];
    let path = Path::new(parts[1]);
    let user = parts.get(3).copied().filter(|s| *s != "-");
    let group = parts.get(4).copied().filter(|s| *s != "-");
    let arg = parts.get(6).copied();
    let ok = match typ {
        "d" | "D" => std::fs::create_dir_all(path).is_ok(),
        "f" | "F" => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(path, arg.unwrap_or("")).is_ok()
        }
        "w" => std::fs::write(path, arg.unwrap_or("")).is_ok(),
        "L" | "L+" => {
            // L PATH TARGET：目标紧跟路径（无 MODE/USER 字段）
            let target = match parts.get(2).copied() {
                Some(t) => t,
                None => return false,
            };
            let _ = std::fs::remove_file(path);
            std::os::unix::fs::symlink(target, path).is_ok()
        }
        "r" => remove_path(path, false),
        "R" => remove_path(path, true),
        "z" | "Z" => path.exists(),
        _ => return false,
    };
    if !ok {
        return false;
    }
    if let Some(m) = parts.get(2).and_then(|m| u32::from_str_radix(m, 8).ok()) {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(m));
    }
    if user.is_some() || group.is_some() {
        chown(path, user, group);
    }
    true
}

fn remove_path(path: &Path, recursive: bool) -> bool {
    if !path.exists() {
        return true;
    }
    let r = if path.is_dir() {
        if recursive {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_dir(path)
        }
    } else {
        std::fs::remove_file(path)
    };
    r.is_ok()
}

/// 调整属主（getpwnam/getgrnam 解析，解析失败保持原属主）。
fn chown(path: &Path, user: Option<&str>, group: Option<&str>) {
    let uid = user.and_then(|u| {
        let c = std::ffi::CString::new(u).ok()?;
        let p = unsafe { libc::getpwnam(c.as_ptr()) };
        (!p.is_null()).then(|| unsafe { (*p).pw_uid })
    });
    let gid = group.and_then(|g| {
        let c = std::ffi::CString::new(g).ok()?;
        let p = unsafe { libc::getgrnam(c.as_ptr()) };
        (!p.is_null()).then(|| unsafe { (*p).gr_gid })
    });
    if uid.is_none() && gid.is_none() {
        return;
    }
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
    unsafe {
        libc::chown(c.as_ptr(), uid.unwrap_or(u32::MAX), gid.unwrap_or(u32::MAX));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(TMPFILES.name(), "tmpfiles");
        assert!(TMPFILES.help().contains("tmpfiles.d"));
    }

    #[test]
    fn create_dir_file_and_link() {
        let base = format!("/tmp/rbox_tmpfiles_{}", std::process::id());
        let _ = std::fs::remove_dir_all(&base);
        let dir = format!("{}/sub", base);
        assert!(apply_line(&format!("d {} 0755", dir)));
        assert!(Path::new(&dir).is_dir());
        let file = format!("{}/f.txt", dir);
        assert!(apply_line(&format!("f {} 0644 - - - content-here", file)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "content-here");
        let link = format!("{}/link", base);
        assert!(apply_line(&format!("L {} {}", link, file)));
        assert_eq!(std::fs::read_link(&link).unwrap().to_string_lossy(), file);
        assert!(apply_line(&format!("r {}", file)));
        assert!(!Path::new(&file).exists());
        assert!(apply_line(&format!("R {}", base)));
        assert!(!Path::new(&base).exists());
        assert!(!apply_line("b /dev/loop0 0660 - - -"));
    }
}
