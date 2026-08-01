//! `printf` - 格式化输出。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct Printf;
pub static PRINTF: &Printf = &Printf;

impl Applet for Printf {
    fn name(&self) -> &'static str { "printf" }
    fn help(&self) -> &'static str { "printf FORMAT [args] - formatted output" }
    fn run(&self, args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("printf: missing FORMAT");
            return ExitCode::from(1);
        }
        let format = &args[0];
        let format_args = &args[1..];
        let mut arg_idx = 0;
        let mut chars = format.chars().peekable();

        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    // 转义序列
                    match chars.next() {
                        Some('n') => print!("\n"),
                        Some('t') => print!("\t"),
                        Some('r') => print!("\r"),
                        Some('\\') => print!("\\"),
                        Some('"') => print!("\""),
                        Some('0') => print!("\0"),
                        Some(other) => print!("\\{}", other),
                        None => break,
                    }
                }
                '%' => {
                    // 格式说明符
                    match chars.next() {
                        Some('s') => {
                            if arg_idx < format_args.len() {
                                print!("{}", format_args[arg_idx]);
                                arg_idx += 1;
                            }
                        }
                        Some('d') => {
                            if arg_idx < format_args.len() {
                                match format_args[arg_idx].parse::<i64>() {
                                    Ok(n) => print!("{}", n),
                                    Err(_) => print!("0"),
                                }
                                arg_idx += 1;
                            }
                        }
                        Some('x') => {
                            if arg_idx < format_args.len() {
                                match format_args[arg_idx].parse::<u64>() {
                                    Ok(n) => print!("{:x}", n),
                                    Err(_) => print!("0"),
                                }
                                arg_idx += 1;
                            }
                        }
                        Some('c') => {
                            if arg_idx < format_args.len() {
                                if let Some(ch) = format_args[arg_idx].chars().next() {
                                    print!("{}", ch);
                                }
                                arg_idx += 1;
                            }
                        }
                        Some('%') => print!("%"),
                        Some(other) => print!("%{}", other),
                        None => { print!("%"); break; }
                    }
                }
                _ => print!("{}", c),
            }
        }
        ExitCode::SUCCESS
    }
}
