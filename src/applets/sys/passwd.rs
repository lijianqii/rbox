//! `passwd` - 修改用户密码（/etc/shadow）。
//!
//! 用法：passwd [user]
//! 非 root 只能改自己且需验证旧密码；哈希使用 SHA-512 crypt（`$6$`）。

use crate::applet::Applet;
use crate::applets::core::rlogin::{authenticate, crypt_hash, read_password};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::ExitCode;

pub struct Passwd;
pub static PASSWD: &Passwd = &Passwd;

/// 生成 `$6$` + 16 随机字符盐。
pub(crate) fn generate_salt() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789./";
    let mut bytes = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if !ok {
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (std::process::id() as u8).wrapping_add(i as u8);
        }
    }
    let salt: String = bytes
        .iter()
        .map(|b| CHARS[*b as usize % CHARS.len()] as char)
        .collect();
    format!("$6${}", salt)
}

/// 更新 shadow 内容：替换指定用户的密码字段；不存在则追加。
pub(crate) fn update_shadow(content: &str, user: &str, hash: &str) -> String {
    let prefix = format!("{}:", user);
    let mut found = false;
    let mut lines: Vec<String> = content
        .lines()
        .map(|l| {
            if !found && l.starts_with(&prefix) {
                let mut fields: Vec<&str> = l.split(':').collect();
                if fields.len() > 1 {
                    fields[1] = hash;
                    found = true;
                    return fields.join(":");
                }
            }
            l.to_string()
        })
        .collect();
    if !found {
        lines.push(format!("{}:{}:19000:0:99999:7:::", user, hash));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// 写回 shadow（0600 权限）。
pub(crate) fn write_shadow(path: &str, content: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())?;
    f.flush()?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

impl Applet for Passwd {
    fn name(&self) -> &'static str {
        "passwd"
    }
    fn help(&self) -> &'static str {
        "passwd [user] - change user password (writes /etc/shadow)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let cfg = crate::config::load();
        let uid = unsafe { libc::getuid() };
        let user = match args.first() {
            Some(u) => u.clone(),
            None => crate::applets::sys::id::uid_name(uid),
        };
        let self_name = crate::applets::sys::id::uid_name(uid);
        if uid != 0 && user != self_name {
            eprintln!("passwd: Permission denied");
            return ExitCode::FAILURE;
        }
        // 非 root 修改自己：先验证旧密码
        if uid != 0 {
            let _ = write!(std::io::stdout(), "Current password: ");
            let _ = std::io::stdout().flush();
            let Some(old) = read_password(Some(60)) else {
                eprintln!("\npasswd: authentication failed");
                return ExitCode::FAILURE;
            };
            if authenticate(&user, &old).is_none() {
                eprintln!("\npasswd: authentication failed");
                return ExitCode::FAILURE;
            }
        }
        let _ = write!(std::io::stdout(), "New password: ");
        let _ = std::io::stdout().flush();
        let Some(p1) = read_password(Some(60)) else {
            eprintln!("\npasswd: password not changed");
            return ExitCode::FAILURE;
        };
        let _ = write!(std::io::stdout(), "\nRetype new password: ");
        let _ = std::io::stdout().flush();
        let Some(p2) = read_password(Some(60)) else {
            eprintln!("\npasswd: password not changed");
            return ExitCode::FAILURE;
        };
        println!();
        if p1 != p2 {
            eprintln!("passwd: passwords do not match");
            return ExitCode::FAILURE;
        }
        if p1.is_empty() {
            eprintln!("passwd: empty password not allowed");
            return ExitCode::FAILURE;
        }
        let salt = generate_salt();
        let Some(hash) = crypt_hash(&p1, &salt) else {
            eprintln!("passwd: cannot hash password");
            return ExitCode::FAILURE;
        };
        let shadow_path = &cfg.paths.shadow;
        let content = std::fs::read_to_string(shadow_path).unwrap_or_default();
        let updated = update_shadow(&content, &user, &hash);
        match write_shadow(shadow_path, &updated) {
            Ok(()) => {
                println!("passwd: password updated successfully");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("passwd: cannot write {}: {}", shadow_path, e);
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(PASSWD.name(), "passwd");
        assert!(PASSWD.help().contains("password"));
    }

    #[test]
    fn salt_format() {
        let s = generate_salt();
        assert!(s.starts_with("$6$"));
        assert_eq!(s.len(), 3 + 16);
        assert_ne!(generate_salt(), s);
    }

    #[test]
    fn crypt_hash_roundtrip() {
        let salt = generate_salt();
        let hash = crypt_hash("secret", &salt).unwrap();
        assert!(hash.starts_with(&salt));
        assert_eq!(crypt_hash("secret", &hash).unwrap(), hash);
        assert_ne!(crypt_hash("wrong", &hash).unwrap(), hash);
    }

    #[test]
    fn update_shadow_replaces_and_appends() {
        let content = "root:old:19000:0:99999:7:::\nnobody:!:19000:0:99999:7:::\n";
        let out = update_shadow(content, "root", "$6$new");
        assert!(out.contains("root:$6$new:19000"));
        assert!(out.contains("nobody:!:"));
        let out = update_shadow(content, "alice", "$6$x");
        assert!(out.contains("alice:$6$x:19000"));
    }

    #[test]
    fn write_shadow_sets_permissions() {
        let path = format!("/tmp/rbox_shadow_{}", std::process::id());
        write_shadow(&path, "x:y\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&path);
    }
}
