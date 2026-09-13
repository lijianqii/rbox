//! `hostname` - 显示/设置主机名。
//!
//! 用法：hostname [-s] [NAME]
//! `-s` 只显示第一段（域名前缀）；设置主机名需要 root。

use crate::applet::Applet;
use std::ffi::CString;
use std::process::ExitCode;

pub struct Hostname;
pub static HOSTNAME: &Hostname = &Hostname;

/// 读取当前主机名。
pub(crate) fn get_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// 设置主机名（需 root）。
pub(crate) fn set_hostname(name: &str) -> std::io::Result<()> {
    let c =
        CString::new(name).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let rc = unsafe { libc::sethostname(c.as_ptr(), name.len()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

impl Applet for Hostname {
    fn name(&self) -> &'static str {
        "hostname"
    }
    fn help(&self) -> &'static str {
        "hostname [-s] [NAME] - show or set system hostname"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut short = false;
        let mut name: Option<&str> = None;
        for a in args {
            match a.as_str() {
                "-s" | "--short" => short = true,
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("hostname: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                s => name = Some(s),
            }
        }
        if let Some(n) = name {
            return match set_hostname(n) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hostname: cannot set name: {}", e);
                    ExitCode::FAILURE
                }
            };
        }
        match get_hostname() {
            Some(h) => {
                if short {
                    println!("{}", h.split('.').next().unwrap_or(&h));
                } else {
                    println!("{}", h);
                }
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("hostname: cannot read hostname");
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
        assert_eq!(HOSTNAME.name(), "hostname");
        assert!(HOSTNAME.help().contains("hostname"));
    }

    #[test]
    fn reads_hostname() {
        assert!(get_hostname().is_some_and(|h| !h.is_empty()));
        assert_eq!(HOSTNAME.run(&[]), ExitCode::SUCCESS);
        assert_eq!(HOSTNAME.run(&["-s".to_string()]), ExitCode::SUCCESS);
    }

    #[test]
    fn unknown_option_fails() {
        assert_eq!(HOSTNAME.run(&["-x".to_string()]), ExitCode::FAILURE);
    }
}
