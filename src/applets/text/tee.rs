//! `tee` - 从 stdin 读并同时写 stdout 与文件。
//!
//! 用法：tee [-a] [file...]
//! `-a` 追加而非覆盖。任一文件写入失败时返回非零，但继续写其他目标。

use crate::applet::Applet;
use std::io::{Read, Write};
use std::process::ExitCode;

pub struct Tee;
pub static TEE: &Tee = &Tee;

impl Applet for Tee {
    fn name(&self) -> &'static str {
        "tee"
    }
    fn help(&self) -> &'static str {
        "tee [-a] [file...] - copy stdin to stdout and files"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut append = false;
        let mut files: Vec<&str> = Vec::new();
        let mut end_of_options = false;
        for a in args {
            if !end_of_options {
                match a.as_str() {
                    "--" => {
                        end_of_options = true;
                        continue;
                    }
                    "-a" | "--append" => {
                        append = true;
                        continue;
                    }
                    s if s.starts_with('-') && s.len() > 1 => {
                        eprintln!("tee: unknown option: {}", s);
                        return ExitCode::FAILURE;
                    }
                    _ => {}
                }
            }
            files.push(a);
        }

        let mut outs: Vec<std::fs::File> = Vec::new();
        let mut had_error = false;
        for f in &files {
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).write(true);
            if append {
                opts.append(true);
            } else {
                opts.truncate(true);
            }
            match opts.open(f) {
                Ok(file) => outs.push(file),
                Err(e) => {
                    eprintln!("tee: {}: {}", f, e);
                    had_error = true;
                }
            }
        }

        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    eprintln!("tee: read error: {}", e);
                    return ExitCode::FAILURE;
                }
            };
            if out.write_all(&buf[..n]).is_err() {
                had_error = true;
            }
            for f in &mut outs {
                if f.write_all(&buf[..n]).is_err() {
                    had_error = true;
                }
            }
        }
        let _ = out.flush();
        for f in &mut outs {
            let _ = f.flush();
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
        assert_eq!(TEE.name(), "tee");
        assert!(TEE.help().contains("stdin"));
    }

    #[test]
    fn tee_writes_files() {
        let a = format!("/tmp/rbox_tee_a_{}", std::process::id());
        let b = format!("/tmp/rbox_tee_b_{}", std::process::id());
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        // 直接调用 run 时 stdin 为空（测试进程），主要验证文件创建与选项解析
        let rc = TEE.run(&[a.clone(), b.clone()]);
        assert_eq!(rc, ExitCode::SUCCESS);
        assert!(std::path::Path::new(&a).exists());
        assert!(std::path::Path::new(&b).exists());
        // -a 追加不报错
        let rc = TEE.run(&["-a".to_string(), a.clone()]);
        assert_eq!(rc, ExitCode::SUCCESS);
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn tee_bad_option_fails() {
        assert_eq!(TEE.run(&["-x".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn tee_unwritable_file_fails() {
        assert_eq!(
            TEE.run(&["/nonexistent_dir_xyz/f".to_string()]),
            ExitCode::FAILURE
        );
    }
}
