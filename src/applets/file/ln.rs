//! `ln` - 创建链接。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Ln;
pub static LN: &Ln = &Ln;

impl Applet for Ln {
    fn name(&self) -> &'static str { "ln" }
    fn help(&self) -> &'static str { "ln [-s] TARGET LINK - create link (default hard, -s symbolic)" }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut symbolic = false;
        let mut positional: Vec<&String> = Vec::new();

        for a in args {
            match a.as_str() {
                "-s" => symbolic = true,
                "-f" => {} // 强制（目前默认就是覆盖行为）
                "-sf" | "-fs" => symbolic = true,
                s if s.starts_with('-') && s.len() > 1 && s != "--" => {
                    eprintln!("ln: unknown option: {}", s);
                }
                _ => positional.push(a),
            }
        }

        if positional.len() < 2 {
            eprintln!("ln: missing operand");
            eprintln!("usage: ln [-s] TARGET LINK");
            return ExitCode::from(1);
        }

        let target = positional[0];
        let link = positional[1];
        let result = if symbolic {
            // 先删除已存在的链接
            let _ = std::fs::remove_file(link);
            std::os::unix::fs::symlink(target, link)
        } else {
            std::fs::hard_link(target, link)
        };

        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ln: {}", e);
                ExitCode::from(1)
            }
        }
    }
}
