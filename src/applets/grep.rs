//! `grep` - 文本搜索。
use crate::applet::Applet;
use std::io::{Read, Write};
use std::process::ExitCode;

pub struct Grep;
pub static GREP: &Grep = &Grep;

impl Applet for Grep {
    fn name(&self) -> &'static str { "grep" }
    fn help(&self) -> &'static str { "grep [-i] [-n] [-v] PATTERN [file] - search text" }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut ignore_case = false;
        let mut show_line_num = false;
        let mut invert = false;
        let mut positional: Vec<&String> = Vec::new();

        for a in args {
            match a.as_str() {
                "-i" => ignore_case = true,
                "-n" => show_line_num = true,
                "-v" => invert = true,
                "-in" | "-ni" => { ignore_case = true; show_line_num = true; }
                "-iv" | "-vi" => { ignore_case = true; invert = true; }
                "-nv" | "-vn" => { show_line_num = true; invert = true; }
                "-" => {}
                s if s.starts_with('-') && s.len() > 1 && s != "--" => {
                    eprintln!("grep: unknown option: {}", s);
                }
                _ => positional.push(a),
            }
        }

        if positional.is_empty() {
            eprintln!("grep: missing PATTERN");
            return ExitCode::from(2);
        }

        let pattern = positional[0];
        let files: Vec<&String> = positional[1..].to_vec();
        let mut out = std::io::stdout().lock();
        let mut found = false;

        let mut search = |content: &str, fname: Option<&str>| {
            let pat = if ignore_case { pattern.to_lowercase() } else { pattern.to_string() };
            for (i, line) in content.lines().enumerate() {
                let target = if ignore_case { line.to_lowercase() } else { line.to_string() };
                let matches = target.contains(&pat);
                if matches != invert {
                    let prefix = match (show_line_num, fname) {
                        (true, Some(f)) => format!("{}:{}: ", f, i + 1),
                        (true, None) => format!("{}: ", i + 1),
                        (false, Some(f)) => format!("{}: ", f),
                        (false, None) => String::new(),
                    };
                    let _ = writeln!(out, "{}{}", prefix, line);
                    found = true;
                }
            }
        };

        if files.is_empty() {
            let mut buf = String::new();
            if std::io::stdin().lock().read_to_string(&mut buf).is_ok() {
                search(&buf, None);
            }
        } else {
            for f in &files {
                match std::fs::read_to_string(f) {
                    Ok(content) => {
                        let fname = if files.len() > 1 { Some(f.as_str()) } else { None };
                        search(&content, fname);
                    }
                    Err(e) => eprintln!("grep: {}: {}", f, e),
                }
            }
        }

        if found { ExitCode::SUCCESS } else { ExitCode::from(1) }
    }
}
