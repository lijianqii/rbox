//! `rlogin` - 校验用户名/密码并启动用户 shell。
//!
//! 用法：`rlogin [username]`（无参数时先提示输入用户名）。
//!
//! 密码校验（/etc/passwd 密码字段为 `x` 时读取 /etc/shadow）：
//! - 空密码字段：免密登录（任意输入均可）；
//! - `!` / `*` 开头：账户锁定，拒绝登录；
//! - `$5$...`（或任意 `$id$` 格式）：用 libc crypt() 校验（与 glibc/busybox 兼容，
//!   支持 SHA-256/SHA-512/MD5 等）；
//! - 其余：明文比对（兼容旧格式）。
//!
//! 校验通过后：initgroups/setgid/setuid 降权、chdir 到 home（失败回退 `/`）、
//! 设置 USER/LOGNAME/HOME/SHELL 环境变量，最后 exec 用户的 shell。

use crate::applet::Applet;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::ExitCode;

pub struct Login;
pub static LOGIN: &Login = &Login;

/// /etc/passwd 解析结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PasswdEntry {
    pub(crate) name: String,
    /// 密码字段原文（`x` 表示密码在 /etc/shadow）。
    pub(crate) passwd: String,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) home: String,
    pub(crate) shell: String,
}

impl Applet for Login {
    fn name(&self) -> &'static str {
        "rlogin"
    }
    fn help(&self) -> &'static str {
        "rlogin [username] - verify password and start user shell"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let cfg = crate::config::load();
        // 用户名：优先取参数，否则提示输入
        let user = match args.first() {
            Some(u) => u.clone(),
            None => {
                let _ = write!(io::stdout(), "{}", cfg.getty.prompt);
                let _ = io::stdout().flush();
                let mut line = String::new();
                if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
                    return ExitCode::SUCCESS;
                }
                let name = line.trim();
                if name.is_empty() {
                    return ExitCode::FAILURE;
                }
                name.to_string()
            }
        };

        // 读取密码（关闭回显）
        let _ = write!(io::stdout(), "{}", cfg.login.password_prompt);
        let _ = io::stdout().flush();
        let password = read_password();
        let _ = writeln!(io::stdout());

        match authenticate(&user, &password) {
            Some(entry) => match login_shell(&entry) {
                // exec 成功不会返回
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rlogin: cannot start shell: {}", e);
                    ExitCode::FAILURE
                }
            },
            None => {
                let _ = writeln!(io::stdout(), "Login incorrect");
                // 失败延迟（防暴力刷屏）由常驻的 rgetty 处理，这里直接退出
                ExitCode::FAILURE
            }
        }
    }
}

/// 回显关闭守卫：drop 时恢复原始终端设置。
struct EchoGuard {
    fd: i32,
    original: libc::termios,
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
    }
}

/// 读取一行密码（终端上关闭 ECHO；非 tty 时直接读取）。
fn read_password() -> String {
    let fd = libc::STDIN_FILENO;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let guard = if unsafe { libc::tcgetattr(fd, &mut term) } == 0 {
        term.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
        Some(EchoGuard { fd, original: term })
    } else {
        None
    };

    let mut pass = String::new();
    let _ = io::stdin().lock().read_line(&mut pass);
    drop(guard);
    pass.trim_end_matches(['\n', '\r']).to_string()
}

/// 解析 /etc/passwd 内容；跳过空行、注释行和字段不足的行。
pub(crate) fn parse_passwd(content: &str) -> Vec<PasswdEntry> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 7 {
                return None;
            }
            Some(PasswdEntry {
                name: f[0].to_string(),
                passwd: f[1].to_string(),
                uid: f[2].parse().ok()?,
                gid: f[3].parse().ok()?,
                home: f[5].to_string(),
                shell: f[6].to_string(),
            })
        })
        .collect()
}

/// 按用户名查找 passwd 条目。
pub(crate) fn find_passwd_entry<'a>(
    entries: &'a [PasswdEntry],
    user: &str,
) -> Option<&'a PasswdEntry> {
    entries.iter().find(|e| e.name == user)
}

/// 从 /etc/shadow 内容中取指定用户的密码字段（用户不存在返回 None）。
pub(crate) fn shadow_password_of(content: &str, user: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let mut f = line.split(':');
        if f.next()? != user {
            return None;
        }
        f.next().map(|p| p.to_string())
    })
}

/// 读取 /etc/shadow 中指定用户的密码字段（路径可配置）。
fn read_shadow_password(user: &str) -> Option<String> {
    let path = &crate::config::load().paths.shadow;
    let content = std::fs::read_to_string(path).ok()?;
    shadow_password_of(&content, user)
}

/// 校验密码：
/// - stored 为 `x`：读取 shadow 字段；空 = 免密，`!`/`*` = 锁定，`$` 开头 = crypt 哈希，否则明文比对；
/// - stored 为空：免密；
/// - 其余：同上（`$` 开头走 crypt，否则明文比对）。
pub(crate) fn password_matches(stored: &str, shadow: Option<&str>, input: &str) -> bool {
    // 实际密码串：passwd 字段为 x 时使用 shadow 字段
    let actual = if stored == "x" || stored == "X" {
        match shadow {
            Some(sp) => sp,
            None => return false,
        }
    } else {
        stored
    };
    if actual.is_empty() {
        return true;
    }
    if actual.starts_with('!') || actual.starts_with('*') {
        return false;
    }
    if actual.starts_with('$') {
        crypt_verify(input, actual)
    } else {
        actual == input
    }
}

/// 用 libc crypt() 校验密码（glibc libcrypt，与 busybox/标准 shadow 兼容）。
/// salt 传完整存储串（crypt 只解析其中的盐部分）。
fn crypt_verify(password: &str, stored: &str) -> bool {
    let p = match std::ffi::CString::new(password) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let s = match std::ffi::CString::new(stored) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let r = unsafe { crypt(p.as_ptr(), s.as_ptr()) };
    if r.is_null() {
        return false;
    }
    // crypt 返回指向静态缓冲区的指针，立即转换为 String 再比较
    let hash = unsafe { std::ffi::CStr::from_ptr(r) };
    hash.to_str().map(|h| h == stored).unwrap_or(false)
}

// libcrypt 的 crypt(3)：`$5$`/`$6$` 等标准密码哈希。
#[link(name = "crypt")]
unsafe extern "C" {
    fn crypt(
        passwd: *const std::ffi::c_char,
        salt: *const std::ffi::c_char,
    ) -> *mut std::ffi::c_char;
}

/// 认证：用户存在且密码正确时返回 passwd 条目。
fn authenticate(user: &str, password: &str) -> Option<PasswdEntry> {
    let path = &crate::config::load().paths.passwd;
    let content = std::fs::read_to_string(path).ok()?;
    let entries = parse_passwd(&content);
    let entry = find_passwd_entry(&entries, user)?.clone();
    let shadow = read_shadow_password(user);
    if password_matches(&entry.passwd, shadow.as_deref(), password) {
        Some(entry)
    } else {
        None
    }
}

/// 登录成功后的动作：降权、切换目录、设置环境、打印 MOTD、exec shell。
fn login_shell(entry: &PasswdEntry) -> io::Result<()> {
    use std::ffi::CString;

    // 降权：initgroups + setgid + setuid（需要 root 权限）
    let name_c = CString::new(entry.name.as_str())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    unsafe {
        if libc::initgroups(name_c.as_ptr(), entry.gid as libc::gid_t) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::setgid(entry.gid as libc::gid_t) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::setuid(entry.uid as libc::uid_t) != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    // chdir 到 home，失败回退 /
    let home = if std::env::set_current_dir(&entry.home).is_ok() {
        entry.home.clone()
    } else {
        "/".to_string()
    };

    // 用户 shell：passwd 无 shell 字段时用配置的缺省 shell
    let shell = if entry.shell.is_empty() {
        crate::config::load().login.shell.clone()
    } else {
        entry.shell.clone()
    };

    // 设置登录环境（单线程 applet，无并发访问）
    unsafe {
        std::env::set_var("USER", &entry.name);
        std::env::set_var("LOGNAME", &entry.name);
        std::env::set_var("HOME", &home);
        std::env::set_var("SHELL", &shell);
    }

    // 打印 MOTD（路径可配置）
    let motd_path = &crate::config::load().paths.motd;
    if let Ok(motd) = std::fs::read_to_string(motd_path) {
        let _ = write!(io::stdout(), "{}", motd);
        let _ = io::stdout().flush();
    }

    // 通知 rgetty"登录成功"：空闲超时从此刻（进入 shell 前）开始计时
    notify_getty_ready();

    // exec 用户 shell（exec 只在失败时返回）
    Err(std::process::Command::new(shell).exec())
}

/// 若 rgetty 通过 RBOX_LOGIN_NOTIFY_FD 提供了通知管道写端，
/// 写入 1 字节表示登录成功（随后关闭并清除环境变量，不泄漏给 shell）。
fn notify_getty_ready() {
    let fd: i32 = std::env::var("RBOX_LOGIN_NOTIFY_FD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1);
    if fd >= 0 {
        let b: u8 = 1;
        unsafe {
            libc::write(fd, &b as *const u8 as *const libc::c_void, 1);
            libc::close(fd);
        }
        // SAFETY: 单线程 applet，无并发环境变量访问
        unsafe { std::env::remove_var("RBOX_LOGIN_NOTIFY_FD") };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
# comment
root:x:0:0:root:/root:/bin/sh
nobody:x:65534:65534:nobody:/:/bin/sh
bad-line
alice:alicepw:1000:1000:Alice:/home/alice:/bin/sh
";

    const SHADOW: &str = "\
root:root:0:0:0:0:0::
nobody:!:19437:0:99999:7:::
alice::19000:0:99999:7:::
locked:*LK*:19437:0:99999:7:::
";

    #[test]
    fn name_and_help() {
        assert_eq!(LOGIN.name(), "rlogin");
        assert!(LOGIN.help().contains("shell"));
    }

    #[test]
    fn parse_passwd_basic() {
        let entries = parse_passwd(PASSWD);
        assert_eq!(entries.len(), 3);
        let root = &entries[0];
        assert_eq!(root.name, "root");
        assert_eq!(root.passwd, "x");
        assert_eq!(root.uid, 0);
        assert_eq!(root.gid, 0);
        assert_eq!(root.home, "/root");
        assert_eq!(root.shell, "/bin/sh");
        assert_eq!(entries[1].name, "nobody");
        assert_eq!(entries[2].name, "alice");
    }

    #[test]
    fn parse_passwd_skips_comments_and_bad_lines() {
        let entries = parse_passwd("# comment\n\nroot:x:0:0:r:/:/bin/sh\nshort:line\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "root");
    }

    #[test]
    fn find_passwd_entry_works() {
        let entries = parse_passwd(PASSWD);
        assert_eq!(find_passwd_entry(&entries, "root").unwrap().uid, 0);
        assert_eq!(find_passwd_entry(&entries, "ghost"), None);
    }

    #[test]
    fn shadow_password_of_works() {
        assert_eq!(shadow_password_of(SHADOW, "root"), Some("root".to_string()));
        assert_eq!(shadow_password_of(SHADOW, "nobody"), Some("!".to_string()));
        assert_eq!(shadow_password_of(SHADOW, "alice"), Some(String::new()));
        assert_eq!(shadow_password_of(SHADOW, "ghost"), None);
    }

    #[test]
    fn password_matches_plaintext() {
        assert!(password_matches("secret", None, "secret"));
        assert!(!password_matches("secret", None, "wrong"));
        assert!(!password_matches("secret", None, ""));
    }

    #[test]
    fn password_matches_shadow() {
        // x + shadow 明文匹配
        assert!(password_matches("x", Some("root"), "root"));
        assert!(!password_matches("x", Some("root"), "wrong"));
        // x 但 shadow 无该用户
        assert!(!password_matches("x", None, "root"));
    }

    #[test]
    fn password_matches_shadow_empty_is_free() {
        assert!(password_matches("x", Some(""), "anything"));
        assert!(password_matches("x", Some(""), ""));
    }

    #[test]
    fn password_matches_shadow_locked() {
        assert!(!password_matches("x", Some("!"), "anything"));
        assert!(!password_matches("x", Some("*LK*"), "anything"));
    }

    #[test]
    fn password_matches_empty_stored_is_free() {
        assert!(password_matches("", None, "anything"));
    }

    #[test]
    fn password_matches_crypt_hash() {
        // 由 `openssl passwd -5 -salt saltstring root` 生成的标准 SHA-256 crypt
        let hash = "$5$saltstring$YFMF7yiQ9V9eCIH9D6jFVOaMhMNpjWG6qrbzxOBzQO8";
        assert!(password_matches(hash, None, "root"));
        assert!(!password_matches(hash, None, "wrong"));
        // passwd 字段为 x + shadow 存哈希
        assert!(password_matches("x", Some(hash), "root"));
        assert!(!password_matches("x", Some(hash), "nope"));
    }
}
