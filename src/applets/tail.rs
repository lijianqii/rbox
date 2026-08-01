//! `tail` - 输出文件后 N 行。
use crate::applet::Applet;
use std::io::{Read, Write};
use std::process::ExitCode;

pub struct Tail;
pub static TAIL: &Tail = &Tail;

impl Applet for Tail {
    fn name(&self) -> &'static str { "tail" }
    fn help(&self) -> &'static str { "tail [-n N] [file] - print last N lines (default 10)" }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut n: usize = 10;
        let mut files: Vec<&String> = Vec::new();

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-n" => {
                    i += 1;
                    if i < args.len() {
                        n = args[i].parse().unwrap_or(10);
                    }
                }
                s if s.starts_with("-n") && s.len() > 2 => {
                    n = s[2..].parse().unwrap_or(10);
                }
                "-" => {}
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("tail: unknown option: {}", s);
                }
                _ => files.push(&args[i]),
            }
            i += 1;
        }

        let mut out = std::io::stdout().lock();
        let print_tail = |content: &str, n: usize, out: &mut std::io::StdoutLock| {
            let lines: Vec<&str> = content.lines().collect();
            let start = if lines.len() > n { lines.len() - n } else { 0 };
            for line in &lines[start..] {
                let _ = writeln!(out, "{}", line);
            }
        };

        if files.is_empty() {
            let mut buf = String::new();
            if std::io::stdin().lock().read_to_string(&mut buf).is_ok() {
                print_tail(&buf, n, &mut out);
            }
        } else {
            for f in &files {
                if files.len() > 1 {
                    let _ = writeln!(out, "==> {} <==", f);
                }
                match std::fs::read_to_string(f) {
                    Ok(content) => print_tail(&content, n, &mut out),
                    Err(e) => eprintln!("tail: {}: {}", f, e),
                }
            }
        }
        ExitCode::SUCCESS
    }
}
