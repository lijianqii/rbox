//! `mktemp` - 创建唯一的临时文件/目录。
//!
//! 用法：mktemp [-d] [-u] [template]
//! 模板中最后一个 `X` 连续段替换为随机字符；默认 `/tmp/tmp.XXXXXX`。
//! `-d` 创建目录；`-u` 只生成名字不创建（不安全，仅兼容）。

use crate::applet::Applet;
use std::io::Read;
use std::process::ExitCode;

pub struct Mktemp;
pub static MKTEMP: &Mktemp = &Mktemp;

/// 随机字符集（避免易混淆字符）。
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// 生成 n 个随机字符（/dev/urandom 不可用时退化为 pid+时间）。
pub(crate) fn random_suffix(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !ok {
        let seed = std::process::id() as u64
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
        let mut x = seed;
        for b in &mut bytes {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (x >> 33) as u8;
        }
    }
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// 把模板末尾的连续 X 段替换为随机字符。
pub(crate) fn fill_template(template: &str) -> Option<String> {
    let idx = template.rfind('X')?;
    let start = template[..=idx]
        .rfind(|c| c != 'X')
        .map(|i| i + 1)
        .unwrap_or(0);
    let count = idx + 1 - start;
    if count < 3 {
        return None; // 至少 3 个 X，降低碰撞概率
    }
    let mut out = String::with_capacity(template.len());
    out.push_str(&template[..start]);
    out.push_str(&random_suffix(count));
    out.push_str(&template[idx + 1..]);
    Some(out)
}

/// 创建文件或目录；`dry=true` 只返回名字。
pub(crate) fn create_temp(template: &str, dir: bool, dry: bool) -> Result<String, String> {
    for _ in 0..32 {
        let path =
            fill_template(template).ok_or("template must contain at least 3 trailing X's")?;
        if dry {
            return Ok(path);
        }
        let created = if dir {
            std::fs::create_dir(&path).is_ok()
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .is_ok()
        };
        if created {
            return Ok(path);
        }
    }
    Err("cannot create temporary file".to_string())
}

impl Applet for Mktemp {
    fn name(&self) -> &'static str {
        "mktemp"
    }
    fn help(&self) -> &'static str {
        "mktemp [-d] [-u] [template] - create a unique temporary file/directory"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut dir = false;
        let mut dry = false;
        let mut template: Option<&str> = None;
        for a in args {
            match a.as_str() {
                "-d" | "--directory" => dir = true,
                "-u" | "--dry-run" => dry = true,
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("mktemp: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                s => template = Some(s),
            }
        }
        let template = template.unwrap_or("/tmp/tmp.XXXXXX");
        match create_temp(template, dir, dry) {
            Ok(path) => {
                println!("{}", path);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("mktemp: {}", e);
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
        assert_eq!(MKTEMP.name(), "mktemp");
        assert!(MKTEMP.help().contains("temporary"));
    }

    #[test]
    fn template_replacement() {
        let out = fill_template("/tmp/a.XXXXXX").unwrap();
        assert!(out.starts_with("/tmp/a."));
        assert_eq!(out.len(), "/tmp/a.".len() + 6);
        assert!(fill_template("/tmp/no-x").is_none());
        assert!(fill_template("/tmp/XX").is_none());
    }

    #[test]
    fn create_file_and_dir() {
        let f = create_temp("/tmp/rbox_mktemp_XXXXXX", false, false).unwrap();
        assert!(std::path::Path::new(&f).is_file());
        let d = create_temp("/tmp/rbox_mktemp_d_XXXXXX", true, false).unwrap();
        assert!(std::path::Path::new(&d).is_dir());
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_dir(&d);
    }

    #[test]
    fn dry_run_does_not_create() {
        let p = create_temp("/tmp/rbox_mktemp_u_XXXXXX", false, true).unwrap();
        assert!(!std::path::Path::new(&p).exists());
    }

    #[test]
    fn bad_template_fails() {
        assert_eq!(MKTEMP.run(&["/tmp/no-x".to_string()]), ExitCode::FAILURE);
    }
}
