//! `id` - 显示用户/组身份。
//!
//! 用法：id [-u] [-g] [-G] [-n] [user]

use crate::applet::Applet;
use std::ffi::CString;
use std::process::ExitCode;

pub struct Id;
pub static ID: &Id = &Id;

/// 用户身份信息。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IdInfo {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) user: String,
    pub(crate) group: String,
    pub(crate) groups: Vec<(u32, String)>,
}

/// 用户名 -> 名称（找不到时回退数字字符串）。
pub(crate) fn uid_name(uid: u32) -> String {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        uid.to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }
            .to_string_lossy()
            .into_owned()
    }
}

/// 组 id -> 名称。
pub(crate) fn gid_name(gid: u32) -> String {
    let gr = unsafe { libc::getgrgid(gid) };
    if gr.is_null() {
        gid.to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr((*gr).gr_name) }
            .to_string_lossy()
            .into_owned()
    }
}

/// 当前进程身份（getgroups）。
pub(crate) fn current_identity() -> IdInfo {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mut groups = Vec::new();
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if n > 0 {
        let mut buf = vec![0 as libc::gid_t; n as usize];
        let got = unsafe { libc::getgroups(n, buf.as_mut_ptr()) };
        for g in buf.into_iter().take(got.max(0) as usize) {
            groups.push((g, gid_name(g)));
        }
    }
    if !groups.iter().any(|(g, _)| *g == gid) {
        groups.insert(0, (gid, gid_name(gid)));
    }
    IdInfo {
        uid,
        gid,
        user: uid_name(uid),
        group: gid_name(gid),
        groups,
    }
}

/// 指定用户身份（getpwnam + getgrouplist）。
pub(crate) fn user_identity(name: &str) -> Option<IdInfo> {
    let c = CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let uid = unsafe { (*pw).pw_uid };
    let gid = unsafe { (*pw).pw_gid };
    let mut ngroups: libc::c_int = 32;
    let mut groups = vec![0 as libc::gid_t; ngroups as usize];
    unsafe {
        libc::getgrouplist((*pw).pw_name, gid, groups.as_mut_ptr(), &mut ngroups);
    }
    groups.truncate(ngroups.max(0) as usize);
    if groups.is_empty() {
        groups.push(gid);
    }
    Some(IdInfo {
        uid,
        gid,
        user: name.to_string(),
        group: gid_name(gid),
        groups: groups.into_iter().map(|g| (g, gid_name(g))).collect(),
    })
}

impl Applet for Id {
    fn name(&self) -> &'static str {
        "id"
    }
    fn help(&self) -> &'static str {
        "id [-u] [-g] [-G] [-n] [user] - print user/group identity"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut show_user = false;
        let mut show_group = false;
        let mut show_groups = false;
        let mut names = false;
        let mut user: Option<&str> = None;
        for a in args {
            match a.as_str() {
                "-u" => show_user = true,
                "-g" => show_group = true,
                "-G" => show_groups = true,
                "-n" => names = true,
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("id: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                s => user = Some(s),
            }
        }
        let info = match user {
            Some(u) => match user_identity(u) {
                Some(i) => i,
                None => {
                    eprintln!("id: {}: no such user", u);
                    return ExitCode::FAILURE;
                }
            },
            None => current_identity(),
        };
        if show_user {
            println!(
                "{}",
                if names {
                    info.user.clone()
                } else {
                    info.uid.to_string()
                }
            );
        } else if show_group {
            println!(
                "{}",
                if names {
                    info.group.clone()
                } else {
                    info.gid.to_string()
                }
            );
        } else if show_groups {
            let out: Vec<String> = info
                .groups
                .iter()
                .map(|(g, n)| if names { n.clone() } else { g.to_string() })
                .collect();
            println!("{}", out.join(" "));
        } else {
            let groups: Vec<String> = info
                .groups
                .iter()
                .map(|(g, n)| format!("{}({})", g, n))
                .collect();
            println!(
                "uid={}({}) gid={}({}) groups={}",
                info.uid,
                info.user,
                info.gid,
                info.group,
                groups.join(",")
            );
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(ID.name(), "id");
        assert!(ID.help().contains("identity"));
    }

    #[test]
    fn current_identity_has_self_group() {
        let info = current_identity();
        assert!(info.groups.iter().any(|(g, _)| *g == info.gid));
        assert_eq!(info.uid, unsafe { libc::getuid() });
    }

    #[test]
    fn unknown_user_fails() {
        assert_eq!(ID.run(&["no_such_user_xyz".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn flags_succeed() {
        assert_eq!(ID.run(&["-u".to_string()]), ExitCode::SUCCESS);
        assert_eq!(ID.run(&["-g".to_string()]), ExitCode::SUCCESS);
        assert_eq!(ID.run(&["-G".to_string()]), ExitCode::SUCCESS);
        assert_eq!(ID.run(&["-un".to_string()]), ExitCode::FAILURE); // 组合不支持
    }
}
