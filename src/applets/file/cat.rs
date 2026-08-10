//! `cat` - 拼接文件并输出到 stdout。无参数读 stdin。
use crate::applet::Applet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub struct Cat;
pub static CAT: &Cat = &Cat;

/// 从 reader 复制到 writer，返回是否成功。
fn copy_reader<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> bool {
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break true,
            Ok(n) => {
                if writer.write_all(&buf[..n]).is_err() {
                    break false;
                }
            }
            Err(_) => break false,
        }
    }
}

impl Applet for Cat {
    fn name(&self) -> &'static str {
        "cat"
    }
    fn help(&self) -> &'static str {
        "cat [files...] - concatenate files to stdout"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut ok = true;
        let mut out = io::stdout().lock();

        if args.is_empty() {
            let mut stdin = io::stdin();
            ok = copy_reader(&mut stdin, &mut out);
        } else {
            for path in args {
                match File::open(path) {
                    Ok(mut f) => {
                        if !copy_reader(&mut f, &mut out) {
                            eprintln!("cat: {}: write error", path);
                            ok = false;
                        }
                    }
                    Err(e) => {
                        eprintln!("cat: {}: {}", path, e);
                        ok = false;
                    }
                }
            }
        }

        let _ = out.flush();
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn tmpdir() -> String {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = format!("/tmp/rbox_cat_test_{}_{}", std::process::id(), n);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn name_and_help() {
        assert_eq!(CAT.name(), "cat");
        assert!(CAT.help().contains("concatenate"));
    }

    #[test]
    fn cat_single_file() {
        let dir = tmpdir();
        let f = format!("{}/test.txt", dir);
        fs::write(&f, "hello cat").unwrap();
        let args = vec![f.clone()];
        let _ = CAT.run(&args);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cat_nonexistent_file() {
        let args = vec!["/nonexistent_file_xyz".to_string()];
        let _ = CAT.run(&args);
        // Should not panic, prints error to stderr
    }

    #[test]
    fn cat_multiple_files() {
        let dir = tmpdir();
        let f1 = format!("{}/a.txt", dir);
        let f2 = format!("{}/b.txt", dir);
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();
        let args = vec![f1, f2];
        let _ = CAT.run(&args);
        let _ = fs::remove_dir_all(&dir);
    }
}
