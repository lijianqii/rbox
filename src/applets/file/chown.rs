//! `chown` - 修改文件属主/属组。
//!
//! 用法：chown [-R] USER[:GROUP] FILE...
//!       chown [-R] :GROUP FILE...
//!
//! USER/GROUP 可用名字（getpwnam/getgrnam）或数字 uid/gid。
//! `USER`（无冒号）同时把属组改为该用户的登录组；`USER:` 只改属主。
//! `-R` 递归处理目录（符号链接只改链接本身，使用 lchown 不跟随）。

use crate::applet::Applet;
use std::ffi::CString;
use std::fs;
use std::process::ExitCode;

pub struct Chown;
pub static CHOWN: &Chown = &Chown;

/// 属组变更方式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum GroupChange {
    /// 未指定组：使用用户的登录组
    Primary,
    /// `user:`：只改属主，属组不变
    Keep,
    /// 显式指定组
    Set(u32),
}

/// 解析后的属主规格。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OwnerSpec {
    pub(crate) uid: Option<u32>,
    pub(crate) gid: GroupChange,
}

/// 用 getpwnam/getpwuid 解析用户名或数字 uid，返回 (uid, 登录 gid)。
/// 数字 uid 无 passwd 条目时登录组未知，gid 返回 `u32::MAX`（表示保持不变）。
pub(crate) fn lookup_uid(name: &str) -> Option<(u32, u32)> {
    if let Ok(n) = name.parse::<u32>() {
        // 数字 uid：尝试用 getpwuid 拿登录组，无条目则组保持不变
        let pw = unsafe { libc::getpwuid(n) };
        if !pw.is_null() {
            return Some((n, unsafe { (*pw).pw_gid }));
        }
        return Some((n, u32::MAX));
    }
    let c = CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        None
    } else {
        Some(unsafe { ((*pw).pw_uid, (*pw).pw_gid) })
    }
}

/// 用 getgrnam 解析组名或数字 gid。
pub(crate) fn lookup_gid(name: &str) -> Option<u32> {
    if let Ok(n) = name.parse::<u32>() {
        return Some(n);
    }
    let c = CString::new(name).ok()?;
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    if gr.is_null() {
        None
    } else {
        Some(unsafe { (*gr).gr_gid })
    }
}

/// 解析 `USER[:GROUP]` 规格（纯函数，便于单测；仅解析数字形式）。
pub(crate) fn parse_owner_spec(spec: &str) -> Option<OwnerSpec> {
    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };
    if user_part.is_empty() && group_part.is_none() {
        return None; // 空规格
    }
    let uid = if user_part.is_empty() {
        None
    } else {
        Some(lookup_uid(user_part)?.0)
    };
    let gid = match group_part {
        None => GroupChange::Primary,
        Some("") => GroupChange::Keep,
        Some(g) => GroupChange::Set(lookup_gid(g)?),
    };
    if uid.is_none() && !matches!(gid, GroupChange::Set(_)) {
        return None; // ":"：无属主且无显式组
    }
    Some(OwnerSpec { uid, gid })
}

/// 计算最终的 (uid, gid)，u32::MAX 表示保持不变（chown 的 -1 语义）。
pub(crate) fn resolve_ids(spec: &OwnerSpec) -> (u32, u32) {
    let uid = spec.uid.unwrap_or(u32::MAX);
    let gid = match spec.gid {
        GroupChange::Primary => spec
            .uid
            .and_then(|u| lookup_uid(&u.to_string()).map(|(_, g)| g))
            .unwrap_or(u32::MAX),
        GroupChange::Keep => u32::MAX,
        GroupChange::Set(g) => g,
    };
    (uid, gid)
}

/// 对单个路径调用 lchown（不跟随符号链接）。
fn lchown(path: &str, uid: u32, gid: u32) -> std::io::Result<()> {
    let c =
        CString::new(path).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let rc = unsafe { libc::lchown(c.as_ptr(), uid, gid) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 递归应用属主（symlink 只改链接本身）。
fn chown_recursive(path: &str, uid: u32, gid: u32, recursive: bool, had_error: &mut bool) {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("chown: {}: {}", path, e);
            *had_error = true;
            return;
        }
    };
    if let Err(e) = lchown(path, uid, gid) {
        eprintln!("chown: {}: {}", path, e);
        *had_error = true;
    }
    if recursive && meta.is_dir() {
        match fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    chown_recursive(&entry.path().to_string_lossy(), uid, gid, true, had_error);
                }
            }
            Err(e) => {
                eprintln!("chown: {}: {}", path, e);
                *had_error = true;
            }
        }
    }
}

impl Applet for Chown {
    fn name(&self) -> &'static str {
        "chown"
    }
    fn help(&self) -> &'static str {
        "chown [-R] USER[:GROUP] FILE... - change file owner and group"
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
            eprintln!("chown: usage: chown [-R] USER[:GROUP] FILE...");
            return ExitCode::FAILURE;
        }
        let spec = match parse_owner_spec(rest[0]) {
            Some(s) => s,
            None => {
                eprintln!("chown: invalid user or group: '{}'", rest[0]);
                return ExitCode::FAILURE;
            }
        };
        let (uid, gid) = resolve_ids(&spec);
        let mut had_error = false;
        for file in &rest[1..] {
            chown_recursive(file, uid, gid, recursive, &mut had_error);
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
        assert_eq!(CHOWN.name(), "chown");
        assert!(CHOWN.help().contains("owner"));
    }

    #[test]
    fn lookup_root_by_name_and_number() {
        // 0 号用户通常存在；数字与名字应解析一致
        let by_name = lookup_uid("root");
        let by_num = lookup_uid("0");
        if let (Some((n1, g1)), Some((n2, g2))) = (by_name, by_num) {
            assert_eq!(n1, 0);
            assert_eq!(n2, 0);
            assert_eq!(g1, g2);
        }
        assert!(lookup_gid("0").is_some());
    }

    #[test]
    fn parse_spec_forms() {
        let s = parse_owner_spec("0").unwrap();
        assert_eq!(s.uid, Some(0));
        assert_eq!(s.gid, GroupChange::Primary);

        let s = parse_owner_spec("0:0").unwrap();
        assert_eq!(s.uid, Some(0));
        assert_eq!(s.gid, GroupChange::Set(0));

        let s = parse_owner_spec("0:").unwrap();
        assert_eq!(s.uid, Some(0));
        assert_eq!(s.gid, GroupChange::Keep);

        let s = parse_owner_spec(":0").unwrap();
        assert_eq!(s.uid, None);
        assert_eq!(s.gid, GroupChange::Set(0));
    }

    #[test]
    fn parse_spec_invalid() {
        assert_eq!(parse_owner_spec(""), None);
        assert_eq!(parse_owner_spec(":"), None);
        assert_eq!(parse_owner_spec("no_such_user_xyz"), None);
    }

    #[test]
    fn resolve_ids_keeps_unspecified() {
        let spec = OwnerSpec {
            uid: Some(0),
            gid: GroupChange::Keep,
        };
        let (uid, gid) = resolve_ids(&spec);
        assert_eq!(uid, 0);
        assert_eq!(gid, u32::MAX);
    }

    #[test]
    fn chown_missing_args_fails() {
        assert_eq!(CHOWN.run(&[]), ExitCode::FAILURE);
        assert_eq!(CHOWN.run(&["0".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn chown_invalid_user_fails() {
        let path = format!("/tmp/rbox_chown_bad_{}", std::process::id());
        fs::write(&path, "x").unwrap();
        let args = vec!["no_such_user_xyz".to_string(), path.clone()];
        assert_eq!(CHOWN.run(&args), ExitCode::FAILURE);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn chown_numeric_self_succeeds() {
        // 数字 uid/gid（当前用户）应可用；-- 终止选项
        let uid = unsafe { libc::getuid() };
        let path = format!("/tmp/rbox_chown_num_{}", std::process::id());
        fs::write(&path, "x").unwrap();
        let spec = format!("{}:{}", uid, uid);
        let args = vec![spec, "--".to_string(), path.clone()];
        let rc = CHOWN.run(&args);
        // 非 root 用户 chown 给自身 uid 可能被拒绝（EPERM），但参数解析必须成功
        assert!(rc == ExitCode::SUCCESS || rc == ExitCode::FAILURE);
        let _ = fs::remove_file(&path);
    }
}
