//! `cat` - 拼接文件并输出到 stdout。无参数读 stdin。
use crate::applet::Applet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub struct Cat;
pub static CAT: &Cat = &Cat;

impl Applet for Cat {
    fn name(&self) -> &'static str {
        "cat"
    }
    fn help(&self) -> &'static str {
        "cat [files...] - concatenate files to stdout"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut ok = true;

        if args.is_empty() {
            // 读 stdin
            let mut stdin = io::stdin();
            let mut buf = [0u8; 64 * 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if io::stdout().write_all(&buf[..n]).is_err() {
                            ok = false;
                            break;
                        }
                    }
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
        } else {
            for path in args {
                match File::open(path) {
                    Ok(mut f) => {
                        let mut buf = [0u8; 64 * 1024];
                        loop {
                            match f.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    if io::stdout().write_all(&buf[..n]).is_err() {
                                        eprintln!("cat: {}: write error", path);
                                        ok = false;
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("cat: {}: {}", path, e);
                                    ok = false;
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("cat: {}: {}", path, e);
                        ok = false;
                    }
                }
            }
        }

        let _ = io::stdout().flush();
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}
